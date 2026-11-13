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

The remote daemon is started with `homeboy daemon ensure-running --addr 127.0.0.1:0` and
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
on the direct or service-managed path. `--adopt-live-lease` skips
convergence, as do placement reconnects.

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

`homeboy upgrade --upgrade-runner` runs the same convergence decision, but
targets the controller identity the upgrade just installed rather than the
running (pre-upgrade) process's build (#16845). See
[`homeboy upgrade`](../commands/upgrade.md#runner-convergence) for how each
status maps onto the runner upgrade entry.

A plain `runner connect` is therefore the normal way to update a runner, and
operator advice for a runner binary older than the controller names
`homeboy runner connect <runner-id>`. `homeboy runner refresh-homeboy` remains
for explicit refs (`--ref`), selecting an existing binary (`--select`),
downgrades (`--allow-downgrade`), dev builds (`runner dev-sync`), rotating a
daemon that drifted from its configured binary, and as the manual fallback when
convergence `failed`.

## Daemon State Directory

Each runner host has exactly one daemon state directory per runner, shared by
every controller (#18063): `~/.config/homeboy/runner-daemons/<runner>/host`.
Direct SSH launches, every remote `daemon` command, and the service unit's
`HOMEBOY_DAEMON_STATE_DIR` derive it from one function, so a restarted daemon
reopens the job store its predecessor wrote, whichever controller restarted it.

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

1. Writes `~/.config/systemd/user/homeboy-runner-<id>-host.service`. The unit
   runs `homeboy daemon serve --addr 127.0.0.1:0` from the stable link
   `~/.local/share/homeboy/runner-service/<id>/host/homeboy`, in
   `runner-daemons/<id>/host`.
2. Stops the idle controller-started daemon so the unit takes the daemon owner lock.
3. Waits for the unit's lease, then marks the runner `service_managed`.

One host unit serves every controller. Controllers are clients: each keeps its
own session and tunnel, and all of them share one daemon, one job store, one
concurrency limit, and one reservation.

For a service-managed runner:

- **Connect** starts the host unit, installing it from the runner's configured
  homeboy only when the host has none yet, waits for its fresh lease, opens the
  tunnel, and writes the session. The daemon may run another build than this
  controller configured: Lab contract compatibility admits it (#16577), and the
  session records the build that actually runs. Connect takes no runtime
  promotion lease, so attaching never waits behind a running Cook's runtime pin.
- **Disconnect** closes the tunnel and removes the session. The daemon and its
  jobs keep running.
- **Refresh** never rotates daemons. It builds the new binary, drains
  through the active-job guard (which counts every controller's jobs), rewrites
  the unit, repoints the unit's link, and restarts the unit. Because the
  session records the build the shared daemon runs, refresh refuses to select
  an older one unless `--allow-downgrade` is passed. The daemon's own startup recovers anything a previous owner left
  behind; other controllers reconnect with `homeboy runner connect`.

The service needs systemd user lingering on the runner host
(`loginctl enable-linger`) so the unit keeps running without a login session.

Before #18063 each controller ran its own unit,
`homeboy-runner-<id>-<controller-prefix>-<sha256>.service`, with its own link and
state directory. On connect or install, a controller retires its own legacy unit
(unit file, drop-ins, and binary link; the state directory stays) once that
daemon has no active job. A busy legacy daemon keeps running until a later
connect finds it idle. Other controllers' legacy units retire when those
controllers upgrade.

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
