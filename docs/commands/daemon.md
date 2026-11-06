# `homeboy daemon`

Run and inspect the local-only Homeboy HTTP API daemon.

## Synopsis

```sh
homeboy daemon <COMMAND>
```

## Subcommands

- `ensure-running` — return the live, fresh daemon, or start or replace one in the background
- `serve` — run the daemon in this process (what a service manager's unit runs)
- `stop` — gracefully stop the background daemon recorded in the state file
- `status` — show bounded daemon health, active jobs, blockers, and recovery actions; use `--full` for complete evidence
- `recover` — resolve and run the right recovery from the current status report
- `plan` — print the one next lifecycle action (read-only; schema `homeboy/daemon-lifecycle-plan/v1`)

## Daemon lifetime

Automatically started daemons (`ensure-running`, including managed WorkJob
launches) stop after **300 seconds with no active durable jobs**. The daemon
enforces this itself: a `serve` launched with a startup token and not run by a
service manager polls its own lease about once a second, and while it has no
active jobs in its scope and its launcher has exited, the idle window runs.
When it lapses, the daemon takes the lifecycle lock and admission fence that
`daemon stop` takes, re-checks that the lease is still its own and still idle,
and drains through the same path as SIGTERM. If a lifecycle command holds the
lock, it retries after another idle window. An idle daemon whose binary was
replaced stops after 30 seconds instead, and a daemon whose state directory
was removed stops at once. A subsequent command starts a new daemon using the retained durable store. Disposable clients do not need to leave a resident process behind.

`serve` is an explicitly resident launch, appropriate for services. Set
`HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS` to a positive number to select another idle
window for a CLI-launched daemon, or `0` for a resident launch (for example
`HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS=0 homeboy daemon ensure-running` for
workflow cadences). A
`serve` started directly by a service manager lives as long as that manager
keeps it, and runs none of the self-stop checks above. A `serve` started by
hand with no startup token stays resident too.

Every `serve` drains on SIGINT and SIGTERM: it finishes the responses it owns,
releases its lease, and records a `clean_stop` termination.

## One process, `daemon.log`, and termination evidence

A daemon is one process. The CLI launches `homeboy daemon serve` directly,
detached, with a startup token in argv and environment; the launched PID is
the lease PID, and nothing else waits on it. Stopping signals that exact PID
after revalidating its lease and startup token, escalating from SIGTERM to
SIGKILL.

The daemon's stdout and stderr are appended to `<state-dir>/daemon.log`, which
the launching CLI opens. A launch truncates the file first once it has grown
past 4 MiB; it is never rotated. When a launch fails, the startup error's
`daemon_log` detail carries a bounded, redacted tail of what that launch wrote.

`termination.json` holds the last termination evidence: `classification`
(`clean_stop` or `unexpected_exit`), `observed_at`, `lease_id`, `pid`,
`binary_identity`, `active_jobs`, `resource_evidence`, `os_evidence`, and
`stop_requested`. `serve` records its own clean stop and `daemon stop` records
the stop it requested. An unexpected exit is a dead PID behind a stale lease;
no exit status or output is recorded, so look in `daemon.log`.

## Service-managed daemons

A `serve` whose parent is systemd with `INVOCATION_ID` set, or launchd with
`XPC_SERVICE_NAME` set, records that manager in its lease (`service_manager`:
`systemd` or `launchd`). No configuration is involved. Leases written before
this field existed read as unmanaged.

Only the manager starts or replaces a service-managed daemon. Homeboy never
spawns another daemon beside it:

- `daemon ensure-running` returns a fresh managed daemon as it is.
- `daemon ensure-running` and `homeboy upgrade` restart a stale managed daemon
  in place. They send SIGTERM to the exact leased process and wait for the
  manager's replacement lease to report the expected build. If no replacement
  appears, the command fails instead of starting one. The unit must restart on
  exit (for example `Restart=always`).
- `daemon status` and `daemon plan` name the owning manager. If another
  `daemon serve` lost the owner lock to the current holder, status reports it
  as `competing_serve` (the contender, its manager, and the holder that
  refused it) for as long as that holder is current.

On any host, while a live or starting daemon holds `owner.lock`, every start
path refuses to spawn another daemon. The serving daemon records its holder
identity (PID, process start identity, startup token) in `owner.lock`, and
`daemon status` derives liveness from it as `owner_lock`: a free lock with a
present `state.json` proves the lease writer is dead; a held lock names the
live owner. Holding the lock without a live lease blocks replacement. Homeboy
does not scan the process table for daemon candidates.

## One daemon store

Homeboy runs one local daemon per state directory, with one durable SQLite job
store. The state directory is `HOMEBOY_DAEMON_STATE_DIR` when set, otherwise
`<config root>/daemon` (normally `~/.config/homeboy/daemon/`). It must be an
absolute path; a relative one is refused rather than resolved against the
current directory. The lease (`state.json`), job store, `owner.lock`, and
`admission.lock` all live there.

Every reader and writer resolves that same store: `daemon status`, `daemon
plan`, `daemon recover`, `daemon jobs`,
`self`, discovery, and controller completion custody. A second
`ensure-running` while a live daemon owns the store returns that daemon.

The only continuity mechanism is restart-resume: a replacement daemon in the
same directory recovers the store's controller jobs at startup and re-adopts
still-running step-job children.

## Controller completion custody

Controller drivers execute once; result persistence is a separate operation.
A terminal winner is immutable: a delayed success, failure, or cancellation
writer observes that winner without rewriting the queue or appending events.

Before publishing a terminal queue snapshot, the store durably records the
public completion and exact execution claim in its existing SQLite index. If
the snapshot write fails, this completion ledger owns the result and the
finished worker exits. Startup and periodic maintenance retry bounded batches
of terminal projections rather than resuming completed driver side effects.
Blocked entries rotate behind other completions. The retained-owner resource
report includes the ledger's pending count and payload bytes.

Cancellation recorded before completion retains priority. Once completion
custody is durable, a new cancellation request reports that terminal
persistence is pending instead of reopening finished execution.

If storage cannot record either the terminal projection or completion custody,
the one existing worker remains fail-closed with backoff capped at 30 seconds.
Permanent transition/ownership rejections exit immediately. This bounds retry
rate without pretending that unavailable storage durably recorded an outcome.

## Recovery dispatch

`homeboy daemon status` is a bounded operator projection by default. It reports
daemon liveness, reachability, build freshness, active-job summaries,
replacement blockers, and exact authorized recovery commands. It includes only
ownership-relevant coordinates. Use `homeboy daemon status --full` to inspect
the complete state, runtime-path, termination, and job-recovery evidence.

`homeboy daemon status` already computes the repair its own evidence
authorizes and reports it as `freshness.repair_plan`. `homeboy daemon recover`
is the dispatcher for that plan: the status resolves the store, lease, and
protected jobs. Before mutation, recovery re-reads that same authority identity
and refuses with `stale_daemon_recovery_plan` and a dry-run handoff if the
authority or protected job set changed. A restartable stale daemon is replaced
in place through `daemon ensure-running`. After `--yes` applies every
step, a final authoritative status read must prove the selected daemon fresh
before the command succeeds. Nothing is transcribed by hand between commands.

```sh
homeboy daemon recover            # resolve and print the plan (default)
homeboy daemon recover --yes      # resolve and run it
```

Dry run is the default because recovery mutates the daemon that owns the
caller's durable jobs. The output carries the resolved `plan` (each step with
its code, its rendered command, and its argv), the `stale_reason_code` it
matched, the `store_path` and `lease_id` that own that plan, and `next_command`.
A preview, blocked recovery, stale plan, stale postcondition, or unavailable
postcondition read returns a nonzero structured outcome while preserving the
plan and any applied-step evidence.

A report the evidence authorizes nothing for does not produce an empty plan. It
produces the read-only `daemon_diagnose` step and `blocked_on`, stating in the
evidence's own words why no mutation was resolved.

One planner decides both the executable steps and the single next lifecycle
action. `recover` reports that action as `lifecycle_plan`, and `homeboy daemon
plan` prints it: `converged`, `transition` (the authorized repair),
`needs_attestation` (the exact dead-generation jobs startup recovery could not
prove gone), `wait` (a busy stale daemon), or `blocked`. `recover` is the only
recovery verb.

### Startup recovers its dead predecessor

Only the daemon holding `owner.lock` writes `state.json`. So while a `serve`
holds the lock, every other lease stamped on a job in its scope belongs to a
generation that is gone, whether or not that generation's `state.json`
survived. Right after it publishes its own lease, and before it resumes
controller jobs, a starting daemon reconciles every active job in its scope
whose lease is not its own:

- staged direct `runner.exec` jobs are requeued under the new lease;
- a job whose recorded child still runs is protected;
- a job whose recorded child is dead is terminalized with its retained
  evidence, or with the terminal result it recorded;
- controller jobs keep their resume semantics (`recover_controller_jobs`);
- a running job with no recorded child identity is left untouched and reported
  as needing attestation, because nothing in process can prove its workload is
  gone.

Jobs of another daemon scope in the shared ledger are never touched.

A dead daemon is therefore repaired by starting its successor: the next
daemon-backed command does it, `homeboy daemon recover --yes` does it at once
(its plan is `ensure-running`, or a restart of a stale daemon), and on a
controller `homeboy runner connect <runner>` does it for the remote runner.

### `--attest-absent`: jobs with no child identity

A daemon that died before recording a job's child PID leaves that job running
in the store with nothing to check. Startup recovery reports it in
`needs_attestation_job_ids`, `status` lists it with the `needs_attestation`
disposition, and `plan` reports `needs_attestation` with the exact job IDs.
Once you have inspected the host and the workload is gone, attest it:

```sh
homeboy daemon recover --attest-absent <job-id> [<job-id>...]
```

This terminalizes exactly the named jobs and nothing else. Each must be
active, in this daemon scope, owned by a dead generation, and carry no recorded
child identity; if any named job fails a check, the whole set is refused and
nothing changes. The attestation is written into every affected job's durable
event data as `operator_confirmed_workload_processes_absent`, together with any
linked durable run.

It works with or without a live daemon. With the owner lock free, no
generation serves the scope and the command holds the lock for the mutation.
With the lock held, the holder must have published its lease in `state.json`;
that lease is the live generation and its jobs are refused. A holder that has
not published yet is still running its own startup recovery, so the
attestation is refused until it finishes. The attestation is never supplied
automatically: it is unverifiable in process by construction.

## Local HTTP API

The daemon binds to loopback only. `homeboy daemon ensure-running` writes the selected
address and PID to the daemon state file so headless clients can discover it via
`homeboy daemon status`.

Always treat the API as a local UI contract. It is not a hosted or remote
multi-user service.

## Dead-Lease Recovery

When a recorded daemon is stale or unreachable but its PID is still live, an
operator can use a lease-bound local force stop. It never uses the daemon HTTP
endpoint, revalidates the exact persisted lease and zero-job state before every
signal, and refuses while durable jobs are active. It uses Linux `/proc` token
evidence on Linux and the explicit startup-token command argument on other Unix
controllers, with bounded SIGTERM-to-SIGKILL escalation for the exact daemon
PID:

```sh
homeboy daemon stop --force --lease-id <exact-live-lease>
```

When `status` reports a dead lease, `active_job_recovery_evidence` lists each
active job's exact ID, lease, timestamps, terminal evidence, child identity,
and `linked_durable_run_id` plus `linked_durable_run_state` (`terminal`,
`active`, or `unresolved`). Active and unresolved linked runs are reported as
blocking evidence. Status is read-only: it never reconciles or changes durable
jobs.

A dead lease's jobs are reconciled by the next daemon's startup recovery; see
[Startup recovers its dead predecessor](#startup-recovers-its-dead-predecessor)
and [`--attest-absent`](#--attest-absent-jobs-with-no-child-identity).

## Endpoints

Restart behavior: queued staged jobs survive daemon restart; running jobs are
marked failed as stale when the durable store is reopened unless a recorded
terminal result can be recovered. The job store has bounded per-job event
retention and is not a long-term audit archive. Persist important evidence
through Homeboy observations/artifacts.

### Built-in Endpoints

- `GET /health` — daemon health and Homeboy version
- `GET /version` — Homeboy version
- `GET /config/paths` — local Homeboy config paths

### Completed Read-Only Contract Endpoints

These endpoints dispatch through Homeboy's transport-free read-only HTTP API
contract and return the same JSON envelope shape as other daemon responses.

- `GET /components`
- `GET /components/:id`
- `GET /components/:id/status`
- `GET /components/:id/changes`
- `GET /rigs`
- `GET /rigs/:id`
- `POST /rigs/:id/check`
- `GET /runs?kind=bench&component=<id>&rig=<id>&status=<status>&limit=<n>`
- `GET /runs/:id`
- `GET /runs/:id/artifacts`
- `GET /runs/:id/artifacts/sync`
- `GET /runs/:id/artifacts/:artifact_id`
- `GET /runs/:id/artifacts/:artifact_id/content`
- `GET /runs/:id/findings?tool=<tool>&file=<path>&fingerprint=<id>&limit=<n>`
- `GET /bench/runs?component=<id>&rig=<id>&status=<status>&limit=<n>`
- `GET /jobs`
- `GET /jobs/:id`
- `GET /jobs/:id/events`
- `GET /jobs/:id/watch?after_sequence=<n>&limit=<n>`
- `POST /jobs/:id/cancel`
- `GET /tools`
- `GET /tools/:id`
- `POST /tools/:id/run`
- `POST /runner/staging/capabilities` (broker `submit` scope)
- `POST /runner/staging/direct` (broker `submit` scope)

The run readers expose persisted observation-store evidence from previous
analysis runs. They do not start lint, test, bench, or rig work.
Run summaries include `status_note` when a running record appears stale or
cannot be verified with owner metadata, matching the CLI run-history output.

Artifact list/sync responses include a byte-retrieval contract for each record:
`content_available`, `content_url`, `fetch_command`, and `retrieval.mode`.
`retrieval.mode: direct_download` means the daemon route can serve bytes and the
CLI command can fetch them. `retrieval.mode: metadata_only` means orchestrators
must treat the record as evidence metadata only; no byte endpoint is expected to
work for that artifact. Daemon artifact byte routes stream the file response;
the transport-free API handler reports inline byte payloads as
`retrieval.mode: inline_base64` with `content_field: content_base64`.

`homeboy runs compare --format=json` remains CLI-only for now. A daemon compare
endpoint should reuse that implementation rather than duplicating comparison
logic in the HTTP API contract.

The analysis entry points `POST /lint`, `POST /test`, and
`POST /bench` enqueue daemon jobs. Clients inspect those jobs through
`GET /jobs/:id` and `GET /jobs/:id/events` instead of parsing terminal output.

Sandbox agents should prefer the typed tool surface over command-shaped routes:

- `GET /tools` returns the bounded Homeboy tool allowlist.
- Each tool declares its required capability, risk category, job behavior, and
  accepted JSON request fields.
- `POST /tools/homeboy.lint/run`, `POST /tools/homeboy.test/run`,
  `POST /tools/homeboy.bench/run`, `POST /tools/homeboy.build/run`, and `POST /tools/homeboy.review/run` enqueue
  jobs through the same job/event/result contract.
- Tool IDs that are not in the allowlist, including deploy, release, SSH, auth,
  keychain, and DB operations, are rejected before execution.

Mutating operations such as deploy, release, rig up/down, git
writes, and SSH execution are not exposed by this daemon slice.

See [Headless Daemon API Contract](../architecture/headless-daemon-api.md) for
the headless client contract, job/event shape, mutating capability model, and
preview/apply rules for future write endpoints.

## Related

- [self](self.md)
- [status](status.md)

## Workflow cadences

A running daemon admits due [workflow cadences](workflow.md#cadence-every) without an external timer. The admitting daemon's orchestration tick (every 60 seconds by default, `HOMEBOY_DAEMON_ORCHESTRATION_TICK_SECS`) admits one instance per due slot; `0` disables the tick and with it every cadence. Registrations live in the observation store (`homeboy-vN.sqlite` under the data directory), beside the job ledger, so they survive daemon restarts. A schema bump starts a fresh store, which drops registrations and in-flight jobs. At start the daemon registers the built-in `homeboy.retention` cadence unless the operator cancelled it.
