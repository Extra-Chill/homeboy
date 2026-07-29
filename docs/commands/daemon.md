# `homeboy daemon`

Run and inspect the local-only Homeboy HTTP API daemon.

## Synopsis

```sh
homeboy daemon <COMMAND>
```

## Subcommands

- `start` — start the local daemon in the background
- `serve` — run the daemon in the foreground
- `stop` — gracefully stop the background daemon recorded in the state file
- `status` — show daemon state, active-job recovery evidence, and selected local address
- `broker-config` — render a deployable reverse-runner broker service recipe

## Local HTTP API

The daemon binds to loopback only. `homeboy daemon start` writes the selected
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
controllers, with bounded SIGTERM-to-SIGKILL escalation for the exact supervised
daemon pair:

```sh
homeboy daemon stop --force --lease-id <exact-live-lease>
```

When `status` reports a dead lease, `active_job_recovery_evidence` lists each
active job's exact ID, lease, timestamps, terminal evidence, child identity,
and `linked_durable_run_id` plus `linked_durable_run_state` (`terminal`,
`active`, or `unresolved`). Active and unresolved linked runs are reported as
blocking evidence. Status is read-only: it never reconciles or changes durable
jobs.

For a legacy job without persisted child identity, use the exact-evidence
recovery command. It validates the persisted daemon lease, recorded daemon PID,
recorded endpoint, job ID, child PID, and Linux child starttime ticks before it
can mutate the one selected job:

```sh
homeboy daemon recover-missing-child-identity \
  --lease-id <expected-lease> \
  --recorded-daemon-pid <recorded-daemon-pid> \
  --recorded-daemon-endpoint <recorded-daemon-endpoint> \
  --job-id <job-id> \
  --child-pid <child-pid> \
  --child-starttime-ticks <child-starttime-ticks>
```

The released `adopt-orphan --recover-missing-child-identity` and
`--confirm-untracked-child-dead <job-id>` flags remain accepted migration
aliases. They must be supplied together when used, return the exact command and
all required evidence fields above, and never mutate jobs.

For a proven unexpected daemon exit where exact active jobs have no persisted
child identity, use the explicit all-active-job-set recovery command. It requires
the dead lease, every active job ID, and an operator attestation that workload
processes were inspected and absent. It refuses a live or reused daemon PID,
missing or mismatched unexpected-exit evidence, a held daemon owner lock,
conflicting daemon-process evidence, child identities, or an omitted/extra/
non-active job ID. Each named job receives durable typed daemon-loss failure
evidence before the replacement daemon starts:

```sh
homeboy daemon reconcile-dead-lease-orphans \
  --lease-id <exact-dead-lease> \
  --job-id <active-job-id> \
  --confirm-workload-processes-absent
```

`--job-id` is not a transcription of `status` output. The store recomputes the
active durable-job set and refuses any mismatch, so the repeated flag is a
compare-and-swap over the exact destructive scope — omit a job, name an extra
one, or race a change and the command aborts instead of terminalizing work you
never saw. The named set is persisted with the reconciliation as
`exact_active_job_set`.

`--confirm-workload-processes-absent` stays required. This command exists
precisely because the daemon died before persisting any child identity, so the
store holds no PID for the named jobs and homeboy cannot observe whether their
workloads are still running. The check that refuses jobs carrying recorded child
evidence proves only that no such record exists — which is what makes the
operator's inspection the sole source of truth. The attestation is written into
every affected job's durable event data as
`operator_confirmed_workload_processes_absent`.

## Deprecated confirmation flags

`--confirm-pid-dead`, `--confirm-no-daemon-owner`, and
`--confirm-control-plane-lost` are deprecated no-ops, retained for one release
and then removed. Every fact they asserted is established by the lifecycle
controller *before* it mutates anything, and the old gates ran ahead of that
verification, so they could only reject correct operators:

| Deprecated flag | Commands | What proves it instead |
| --- | --- | --- |
| `--confirm-pid-dead` | `adopt-orphan`, `reconcile-dead-lease-orphans`, `recover-missing-lease-state` | A `pid_dead` freshness code, a non-running recorded PID, and — for adoption and dead-lease recovery — a second liveness proof taken under the daemon owner lock, so a reused PID cannot slip through. Dead-lease recovery additionally requires persisted unexpected-termination evidence bound to the exact lease and PID. |
| `--confirm-no-daemon-owner` | `reconcile-leaseless-orphans` | The daemon owner lock (refused while any daemon is live or starting), a fail-closed daemon-process candidate probe, and a fail-closed listener probe at `--addr`. |
| `--confirm-control-plane-lost` | `recover-missing-lease-state` | An absent daemon state record, a `lease_missing` freshness code, an unreachable daemon, active jobs, and a failed connect to the recorded endpoint. |

Passing them still works and changes nothing. Drop them from scripts and
runbooks. The same three flags are deprecated on `homeboy runner connect`, where
supplying one *without* its recovery mode is still refused — a confirmation
selects no recovery on its own.

`--confirm-no-daemon-owner` intentionally remains visible, and with no help text,
in `homeboy daemon reconcile-leaseless-orphans --help`: controllers negotiate the
remote lease-less recovery contract by parsing bare long options out of that help
output. Do not hide it or give it a doc comment before the flag is removed
outright.

## VPS Reverse Runner Broker

`homeboy daemon broker-config` renders the code-backed deployment shape for a
VPS-hosted reverse runner broker. The safe default is a durable `systemd`
service that keeps the daemon on a stable loopback port:

```sh
homeboy daemon broker-config --listen-addr 127.0.0.1:7421
```

The JSON output includes:

- `systemd_unit` for a `homeboy-broker` service running `homeboy daemon serve`
- `private_tunnel_examples` for SSH, Cloudflare, or tailnet-only access
- optional `nginx_site` and `caddy_site` snippets when `--domain` is supplied
- `daemon_state_path` and `daemon_jobs_path` service-owned operational state locations
- status and log commands for day-two operations
- restart, retention, and claim caveats

The service config intentionally requires a stable loopback address. Broker
routes are currently suitable for private loopback or private tunnel access only.
Public Internet exposure through Nginx or Caddy is blocked until broker
auth/pairing from [#2990](https://github.com/Extra-Chill/homeboy/issues/2990)
lands. The rendered proxy snippets include that warning and should stay disabled
or protected by private network controls until the auth model is available.

Extra Chill-compatible private setup:

1. Install Homeboy on the VPS at the binary path used in `broker-config`.
2. Create the service user/group named in the generated output.
3. Install the rendered `systemd_unit` as `/etc/systemd/system/homeboy-broker.service`.
4. Run `systemctl daemon-reload && systemctl enable --now homeboy-broker`.
5. Verify with `systemctl status homeboy-broker`, `homeboy daemon status`, and `curl -fsS http://127.0.0.1:7421/health` on the VPS.
6. Reach the broker from the runner machine through a private SSH tunnel or private network URL, then use reverse runner connection commands against that private broker URL.

Operational caveats:

- The systemd service sets `HOME=/var/lib/homeboy`, so daemon state lives under `/var/lib/homeboy/.config/homeboy/daemon/` instead of the service user's login home.
- Queued reverse-runner jobs survive daemon restart.
- Broker-owned running jobs are marked failed as stale when the durable store is reopened after restart.
- Active reverse-runner claims are lease-scoped; runners should retry claim after the lease expires.
- The job store has bounded per-job event retention and is not a long-term audit archive. Persist important evidence through Homeboy observations/artifacts.

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
- `GET /stacks`
- `GET /stacks/:id`
- `POST /stacks/:id/status`
- `GET /runs?kind=bench|audit&component=<id>&rig=<id>&status=<status>&limit=<n>`
- `GET /runs/:id`
- `GET /runs/:id/artifacts`
- `GET /runs/:id/artifacts/sync`
- `GET /runs/:id/artifacts/:artifact_id`
- `GET /runs/:id/artifacts/:artifact_id/content`
- `GET /runs/:id/findings?tool=<tool>&file=<path>&fingerprint=<id>&limit=<n>`
- `GET /audit/runs?component=<id>&rig=<id>&status=<status>&limit=<n>`
- `GET /bench/runs?component=<id>&rig=<id>&status=<status>&limit=<n>`
- `GET /jobs`
- `GET /jobs/:id`
- `GET /jobs/:id/events`
- `POST /jobs/:id/cancel`
- `GET /tools`
- `GET /tools/:id`
- `POST /tools/:id/run`
- `POST /runner/sessions`
- `POST /runner/jobs`
- `POST /runner/jobs/claim`
- `POST /runner/jobs/:id/events`
- `POST /runner/jobs/:id/finish`

The run readers expose persisted observation-store evidence from previous
analysis runs. They do not start audit, lint, test, bench, rig, or stack work.
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

The analysis entry points `POST /audit`, `POST /lint`, `POST /test`, and
`POST /bench` enqueue daemon jobs. Clients inspect those jobs through
`GET /jobs/:id` and `GET /jobs/:id/events` instead of parsing terminal output.

Sandbox agents should prefer the typed tool surface over command-shaped routes:

- `GET /tools` returns the bounded Homeboy tool allowlist.
- Each tool declares its required capability, risk category, job behavior, and
  accepted JSON request fields.
- `POST /tools/homeboy.audit/run`, `POST /tools/homeboy.lint/run`,
  `POST /tools/homeboy.test/run`, `POST /tools/homeboy.bench/run`,
  `POST /tools/homeboy.build/run`, and `POST /tools/homeboy.review/run` enqueue
  jobs through the same job/event/result contract.
- Tool IDs that are not in the allowlist, including deploy, release, SSH, auth,
  keychain, and DB operations, are rejected before execution.

Mutating operations such as deploy, release, rig up/down, stack apply, git
writes, and SSH execution are not exposed by this daemon slice.

See [Headless Daemon API Contract](../architecture/headless-daemon-api.md) for
the headless client contract, job/event shape, mutating capability model, and
preview/apply rules for future write endpoints.

## Related

- [self](self.md)
- [status](status.md)

## Scheduled runs

A running daemon fires due [schedules](schedule.md) without an external timer. It polls every 30 seconds by default; override with `HOMEBOY_DAEMON_SCHEDULE_TICK_SECS`, or set it to `0` to disable daemon-driven scheduling and drive `homeboy schedule tick` yourself.

Each due schedule runs on its own thread, so a slow scheduled command delays neither the poll loop nor daemon shutdown. Markers left by a run the daemon did not finish are reclaimed at start.
