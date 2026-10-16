# Headless Daemon API Contract

Homeboy is CLI-first, but the daemon is the stable local UI and automation
surface for clients that should not shell out and parse terminal output. The
daemon remains a local Homeboy engine, not a hosted control plane.

## Scope

The daemon owns a loopback-only HTTP contract for:

- local client discovery and health checks
- read-only component, rig, run, artifact, and finding inspection
- long-running lint, test, and bench jobs
- typed, allowlisted sandbox-agent Homeboy tool jobs
- structured job events and final results
- future mutating operations behind explicit capabilities and confirmations

Direct SSH runner staging adds two broker-authenticated routes that a
controller reaches through its SSH loopback tunnel:

- `POST /runner/staging/capabilities` to discover the staged-execution protocol
- `POST /runner/staging/direct` to stage a sealed execution and run it on the
  daemon's local child lifecycle

Controllers then observe the job through the local `/jobs/:id` routes, including
`GET /jobs/:id/watch`. These routes are not safe as an unauthenticated public
service; reach them only over loopback plus a private SSH tunnel.

The CLI remains usable without the daemon. Daemon routes reuse Homeboy core and
command adapters so desktop, web, agent, and CLI workflows do not fork product
logic.

## Discovery

Clients discover a local daemon through the CLI-managed state file instead of a
hardcoded public port.

```text
homeboy daemon start
        |
        +-- binds 127.0.0.1:<port>
        +-- writes pid/address/token metadata to the daemon state file
        |
homeboy daemon status
        |
        +-- returns the current loopback address and process state
```

Remote runner clients use `homeboy runner connect <runner-id>` to create an SSH
loopback tunnel to a runner daemon. The client still talks to a local loopback
URL; Homeboy owns the SSH tunnel and rejects non-loopback daemon addresses.


## Minimum Dashboard Surface

A useful headless UI can be built from this read/query surface:

- `GET /health` and `GET /version` for connectivity and compatibility
- `GET /config/paths` for local configuration discovery
- `GET /components`, `GET /components/:id`, `GET /components/:id/status`, and
  `GET /components/:id/changes` for source checkout selection
- `GET /rigs`, `GET /rigs/:id`, and `POST /rigs/:id/check` for environment
  readiness
- `GET /runs`, `GET /runs/:id`, `GET /runs/:id/artifacts`,
  `GET /runs/:id/artifacts/sync`, `GET /runs/:id/artifacts/:artifact_id`,
  `GET /runs/:id/artifacts/:artifact_id/content`, and `GET /runs/:id/findings`
  for persisted evidence
- `GET /bench/runs` for analysis-specific run history
- `GET /v1/control-plane/capabilities`, `GET /v1/control-plane/missions`,
  `GET /v1/control-plane/missions/:id`, `GET /v1/control-plane/runs`,
  `POST /v1/control-plane/runs`, `GET /v1/control-plane/runs/:id`,
  `GET /v1/control-plane/runs/:id/tasks`, and
  `GET /v1/control-plane/runs/:id/tasks/:task_id`, plus nested attempt list and
  detail routes below each task, for the
  typed orchestration service. Mission and run discovery accept bounded
  `limit` values and opaque keyset cursors. Run discovery accepts an optional
  typed `mission` filter, and continuation cursors are bound to that filter.
  Run-list rows are summaries: they omit `action_eligibility`, which is
  computed only by the single-run read (`GET /v1/control-plane/runs/:id`).
  Clients that render or execute actions fetch the run itself.
  Tasks are addressed within their parent run because persisted task names are
  run-local; task pagination cursors are likewise bound to the run.
  Provider attempts use their durable `{run}:{task}:{attempt}` owner identity;
  attempt cursors bind to both parent identities.
  New provider reservations persist a distinct canonical execution identity and
  expose it through nested execution list/detail routes below the attempt.
  Runner jobs, provider sessions, and process IDs remain external references;
  records predating the execution identity remain visible with no inferred
  execution resource.
  Run-scoped `/artifacts`, `/evidence`, and `/external-references` routes expose
  list, detail, and `POST` registration operations. Automatic lifecycle
  artifact/evidence pointers and explicitly registered references share one
  typed read model with content-derived automatic identities. Registrations are
  bounded, actor-attributed, and exactly replayable by a persisted digest of the
  idempotency key. Reference URLs are query-redacted and stripped of fragments
  before persistence and projection; replay keys are not exposed by reads.
  Network writes require the same paired broker `submit` scope as run submission.
  `POST /v1/control-plane/runs/:id/events` appends caller-owned events to a
  dedicated durable ledger. The controller assigns the event ID and monotonic
  run-local sequence, persists only a digest of the idempotency key, validates
  nested identities against the run graph, and redacts bounded payloads and
  references before persistence. Replays return the original event, including
  after its payload ages out of the retained read window; conflicting reuse of
  a key is rejected. Synthesized `agent-task logs` remain a separate legacy
  projection rather than being frozen into this forward-only ledger.
  Control-plane actions append one `action.accepted` event before invoking the
  delegated effect and one terminal `action.succeeded`,
  `action.already_satisfied`, or `action.failed` event after persisting the
  immutable acknowledgement. Matching action replays return that acknowledgement
  without appending duplicate events; conflicting intent is rejected before an
  event or effect. Internal action event identities are reserved from caller-owned
  append requests. Compact receipts suppress the redacted legacy action-log
  projection even after event payload retention expires.
  Event pages reject non-monotonic, duplicate, or cross-run streams. Their
  opaque cursors are bound to one run; the adjacent `/events/retention` resource
  reports the earliest and latest retained sequence without changing the strict
  v1 page shape. A cursor older than that window returns the typed
  `cursor_expired` error with HTTP `410 Gone`.
  The mission index is forward-only:
  canonical mission ownership is indexed transactionally with each new run
  projection after the index schema is installed. HTTP submission requires broker `submit` scope,
  binds the durable actor to that credential, and queues a controller plan
  already staged under its canonical run ID. Provider, model, runner, and plan
  payloads remain adapter-owned, and claimed execution stays off the serial
  HTTP request loop. Capabilities advertise only the operations wired in this
  build. Discovery, exact retrieval, and `homeboy agent-task status` are pure
  reads; live runner refresh and durable repair belong to the explicit
  reconciliation operation. The exact route accepts run ids; mission/Cook ids
  belong to the mission resource rather than acting as run aliases. A run is
  not a job — the job *supervises* the run, so
  watching and cancelling stay on the controller-job surface below.
- `GET /jobs`, `GET /jobs/:id`, `GET /jobs/:id/events`, and
  `POST /jobs/:id/cancel` for long-running work. Cook and fanout are both
  controller jobs, so a detached cook or wave is already visible and
  cancellable here — but through `POST /controller/jobs/:id/cancel`, not
  `POST /jobs/:id/cancel`, which fails closed on a controller job so the
  driver can stop the work it owns.
  A detached Cook is a `step` controller job
  (`homeboy/step-job-request/v1`): the daemon spawns its argv with
  `env_clear()` plus the environment the launcher admitted (public values and
  secret references only), supervises the child by PID and start identity,
  writes its output to `artifacts/jobs/<job_id>/step.log`, and terminates the
  child's whole process tree on cancellation. A step may wait for
  `not_before` or `runner_eligible` before it spawns.
- `GET /tools`, `GET /tools/:id`, and `POST /tools/:id/run` for sandbox agents
  that need typed Homeboy tool execution without arbitrary shell access

This is enough for a dashboard that lists components, shows selected checkout
state, displays rigs, starts analysis jobs, streams progress, and renders
structured final results.

Artifact evidence is intentionally explicit about byte availability. Artifact
metadata and sync payloads expose `content_available`, `content_url`,
`fetch_command`, and `retrieval.mode`. Consumers fetch bytes only when
`content_available` is `true`; `metadata_only` records are evidence pointers and
must not trigger guessed runner paths or private filesystem probing. The
transport-free API handler returns JSON with `retrieval.mode: inline_base64` and
the bytes in `content_base64`; daemon artifact byte routes stream the file
response directly when serving artifact responses.

## Job/Event Contract

Long-running analysis routes return immediately with a job record. Clients poll
job state and events instead of holding a request open.

```text
POST /lint|/test|/bench
        |
        v
{ "job": { "id": "...", "status": "queued" } }
        |
        +--> GET /jobs/:id
        +--> GET /jobs/:id/events
        +--> POST /jobs/:id/cancel
```

Events are append-only records. They are safe for UIs to render incrementally and
safe for runners to mirror as evidence. The final result event carries the same
structured result shape as the corresponding CLI command, including artifacts,
findings, summaries, and CI context when a CI profile/job selector was used.

## Sandbox Tool Surface

Sandbox agents use `/tools` instead of `/exec`. The surface is an allowlist, not a
shell command proxy.

```text
GET /tools
        |
        +-- [{ id, command, required_capability, risk, runs_as_job,
              allowed_arguments }]

POST /tools/homeboy.review/run
        |
        +-- validates the tool id and JSON body fields
        +-- enqueues a daemon job
        +-- returns poll links for /jobs/:id and /jobs/:id/events
```

The first bounded slice exposes these executable tool IDs:

- `homeboy.lint` with `run:lint`
- `homeboy.test` with `run:test`
- `homeboy.bench` with `run:bench`
- `homeboy.build` with `run:build`
- `homeboy.review` with `run:review`

Each tool accepts only its declared JSON arguments. Mutating or operator-shaped
fields such as lint `fix`, baseline writes, ratchets, free-form build JSON,
review markdown report output, and review banners are rejected. Non-allowlisted
tool IDs such as deploy, release, SSH, auth, keychain, and DB operations are
rejected before any job starts.

`POST /exec` remains a daemon-internal structured execution route for runner
plumbing. It accepts only a Runner API v1 `submission` envelope with a
caller-owned `submission_key`; a body without one is refused with a
`submission` validation error. `POST /admissions` likewise requires
`admission_lease_protocol: 1` and returns a renewable token-bearing lease. It is
not the sandbox-agent contract.

The daemon durable job store is the observation store
(`homeboy-v32.sqlite` under the Homeboy data directory). The ledger keeps
bounded per-job events and supports restart recovery for queued staged jobs. It
is operational state, not a durable audit archive; important evidence should
still be persisted as Homeboy observations or artifacts.

Restart behavior:

- queued staged jobs stay queued across daemon restart
- running jobs are marked failed as stale when the store is reopened, unless a
  recorded terminal result can be recovered

## Client Replacement Order

Headless clients should replace shell-out flows in low-risk order:

1. Discovery: `daemon status`, `health`, and `version`.
2. Dashboard reads: components, status, changes, rigs, runs, artifacts,
   and findings.
3. Analysis jobs: lint, test, and bench with event polling.
4. Safe cancellation: `POST /jobs/:id/cancel` for daemon-owned queued/running
   jobs.
5. Mutating actions only after the capability and confirmation model below is
   implemented.

## Mutating Endpoint Model

The daemon defaults to no write/operator capabilities. Future mutating endpoints
must declare capability requirements before they are routed.

Suggested capability names:

- `read:components`
- `read:runs`
- `run:lint`
- `run:test`
- `run:bench`
- `write:files`
- `write:git`
- `write:rig`
- `operator:ssh`
- `operator:deploy`
- `operator:release`

Risk categories:

- Read-only: status, inventory, persisted runs, artifacts, findings.
- Bounded local run: lint, test, bench, rig check.
- Local write: file edits, refactor fixes, generated patches.
- Git write: commit, tag, push.
- Environment write: rig up/down/service operations.
- Operator write: SSH execution, deploy, release, transfer, DB operations.

High-risk operations use a preview/apply contract:

```text
POST /operations/preview
        |
        +-- validates capability
        +-- returns operation_id, required_capabilities, risk, and plan/diff

POST /operations/:id/apply
        |
        +-- validates the same capabilities
        +-- applies exactly the previewed plan
        +-- runs as a job and emits events/results
```

The apply request must not accept a new free-form plan body. It applies the
stored preview by id so clients cannot accidentally confirm one plan and execute
another.

## Default-Deny Rules

- Bind to loopback by default.
- Reach broker-authenticated `/runner/*` routes only through loopback or a private SSH tunnel.
- Treat daemon tokens/capabilities as local secrets.
- Reject mutating routes until they declare required capabilities.
- Require preview/apply for high-risk writes.
- Run long writes as jobs with event trails and artifacts.
- Return enough structured data for review or undo where Homeboy has an undo
  primitive.

## Related Issues

- [#1759](https://github.com/Extra-Chill/homeboy/issues/1759)
- [#1761](https://github.com/Extra-Chill/homeboy/issues/1761)
- [#1762](https://github.com/Extra-Chill/homeboy/issues/1762)
