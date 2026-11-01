# Runner Connection Bootstrap

`homeboy runner connect <runner-id>` uses the first-class runner registry added
for issue #2526.

## Registry Contract

Runner configs are JSON files at `~/.config/homeboy/runners/<id>.json` with the
initial #2526 shape:

```json
{
  "kind": "ssh",
  "server_id": "lab-box",
  "workspace_root": "/srv/homeboy-lab",
  "homeboy_path": "homeboy",
  "daemon": true
}
```

Only `kind: "ssh"`, `server_id`, and optional `homeboy_path` are used by the
Wave 1 connection commands. Registry CRUD owns creation, validation, and future
execution metadata such as `workspace_root`, `concurrency_limit`, `env`, and
`resources`.

## Connection Shape

The remote daemon is started with `homeboy daemon start --addr 127.0.0.1:0` and
the reported address is rejected unless it is loopback. The local client reaches
the daemon through an SSH `-L 127.0.0.1:<local>:127.0.0.1:<remote>` tunnel.

Session metadata is stored at `~/.config/homeboy/runner-sessions/<id>.json` so
`status` and `disconnect` can inspect or close the local tunnel later.

Before a direct-SSH connect is reported as successful, Homeboy captures the
local tunnel process start identity and observes it through a bounded
post-establishment window while the exact remote lease/PID continues answering
`/health`. A failed observation cleans only that exact owned child. Connect
failure evidence may add `durability_stage`; existing health attempt fields are
preserved. `health_attempt_count` and `health_attempts` contain failed initial
health probes and failed durability observations, not successful observations.

## Binary Convergence

A plain `homeboy runner connect <runner-id>` first brings the runner's
configured Homeboy binary to the controller's release (#16825), then attaches
on the direct or service-managed path. Explicit recovery flags
(`--adopt-orphan-lease`, `--adopt-live-lease`, `--reconcile-*`,
`--recover-missing-lease-state`) skip convergence, as do placement reconnects.

Convergence compares the configured binary's `self identity` version with the
controller's build:

| Situation | `binary_convergence.status` |
| --- | --- |
| Controller is not a clean release build (dirty, no commit, pre-release) | `skipped_dev_build` (the runner is not probed) |
| Runner identity unreadable | `skipped_unverifiable` |
| Runner already at the controller's version | `already_current` |
| Runner newer than the controller | `skipped_remote_newer` (never downgraded) |
| Runner older, daemon has active jobs | `pending_active_jobs` with `blocking_job_ids`; connect attaches to the current daemon and a later connect converges |
| Runner older, install succeeded | `converged` |
| Runner older, install failed (network, checksum, target) | `failed` with `detail`; connect proceeds with the existing binary |

Installing reuses `refresh-homeboy`'s verified release path pinned to the
controller's exact tag `v<version>` (never `latest`, never a source build) into
`_homeboy_binaries/homeboy-<sha256>/homeboy`, selects it, and replaces the daemon
with `refresh-homeboy --reconnect`'s mechanism: an in-place restart, or the
service unit repoint and restart. That refresh holds its own runtime promotion
lease and finishes before connect takes its lease; its reconnect attaches
through the non-converging connect, so convergence never recurses.

`homeboy runner refresh-homeboy` remains for explicit refs (`--ref`), dev
builds (`runner dev-sync`), and selecting an existing binary (`--select`).

## Daemon State Directory

Each (runner, controller) pair has exactly one daemon state directory on the
runner host: `~/.config/homeboy/runner-daemons/<runner>/<controller>`. Direct SSH
launches, every remote `daemon` command, and the service unit's
`HOMEBOY_DAEMON_STATE_DIR` derive it from one function, so a restarted daemon
reopens the job store its predecessor wrote.

### One-time move off `daemon-generations`

Homeboy before #16752 kept runner daemons in
`~/.config/homeboy/daemon-generations/<runner>/controllers/<controller>/<dir>`
(`primary` or a rotated 48-hex generation). Before a daemon starts in the new
directory, direct connect, direct in-place restart, `runner service install`,
and service repoint-and-restart run one legacy step: for each subdirectory of
that controller's legacy root it reads `daemon status`, and if any legacy daemon
has active jobs the operation fails with `legacy_daemon_busy`, naming the
directory and job count, without stopping anything or starting a second daemon.
Otherwise it runs the normal `daemon stop` in each directory. A recorded session
whose lease was one of the stopped daemons is dropped, so connect proceeds to
the daemon in the new directory. The legacy directories stay on disk for
operator cleanup. This step exists only to move runners off the old layout
(epic #16680) and will be deleted once no runner has a `daemon-generations`
directory.

## Current Session

A controller tracks exactly one current session per runner. Jobs, runs, logs,
cancellation, and artifacts route through that session's daemon; there is no
ledger of earlier daemons. Replacing the daemon publishes the new session under
the runner state lock only while the recorded lease is still the one being
replaced, and an in-place restart rebinds the same session to the restarted
lease. A stop that cannot prove the remote daemon stopped keeps the session; a
session whose daemon is proven dead is cleared. Controllers no longer read or
write `generations.json` or `retirement-verdicts.state`; files left by earlier
versions are ignored and not deleted.

## Runner-Owned Service

A runner can own its daemon as a systemd user unit (#13881 step 3). The
controller then never starts, replaces, rotates, or retires the remote daemon.

`homeboy runner service install <runner-id>` requires an idle runner. It:

1. Writes `~/.config/systemd/user/homeboy-runner-<id>-<controller>.service`.
   The unit runs `homeboy daemon serve --addr 127.0.0.1:0` from the controller's
   stable link `~/.local/share/homeboy/runner-service/<id>/controllers/<controller>/homeboy`,
   in `runner-daemons/<id>/<controller>`, with `Restart=always`. The
   controller scope is a readable sanitized prefix plus the full SHA-256 of the
   raw controller ID, shared by service names, binary slots, and daemon state
   paths so sanitized IDs cannot alias.
2. Runs the one-time legacy `daemon-generations` step above, then stops the
   idle controller-started daemon so the unit takes the daemon owner lock.
3. Waits for the unit's lease, then marks the runner `service_managed`.

For a service-managed runner:

- **Connect** waits for the service's fresh lease, opens the tunnel, and
  writes the session. It takes no runtime promotion lease, so attaching never
  waits behind a running Cook's runtime pin.
- **Disconnect** closes the tunnel and removes the session. The daemon and its
  jobs keep running.
- **Refresh** never rotates daemons. It builds the new binary, drains
  through the active-job guard, runs the legacy step, rewrites the unit (so a
  unit still pointing at a legacy state directory moves to the new one),
  repoints the unit's link, and restarts the unit. The daemon's own startup recovers anything a previous owner left
  behind.

The service needs systemd user lingering on the runner host
(`loginctl enable-linger`) so the unit keeps running without a login session.

Older releases used the unsuffixed `homeboy-runner-<id>.service`, shared binary
link, and `daemon-generations/<id>/primary`; the intermediate controller-scoped
scheme also used sanitized IDs without a hash. New installs never overwrite,
stop, or adopt either legacy unit name/path. Such a unit may continue owning jobs
while controllers move to hash-scoped units. Retire a legacy unit only after an
operator verifies its exact unit and lease and confirms that it has no active
jobs. Then stop and disable that specific unit; remove its unit file, binary
link, or state directory only when no remaining controller depends on them.
Do not use the new controller-scoped service commands to perform legacy cleanup.

## Unreachable recorded host

Connect dials the configured server host. It does not discover a replacement
address. SSH exit 255 with `Host is down`, or the same resolution/reachability
failure, is `transport_unreachable` with that recorded host and the next check.
That is not a missing Homeboy install.

A numeric DHCP address is not stable. Set a stable hostname, or a DHCP
reservation, on the existing server, then reconnect:

```sh
homeboy server set <server-id> --json '{"host":"<hostname>"}'
```

SSH host key verification stays on. Do not accept a new key.
