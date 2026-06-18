# Controller to runner reverse-runner setup

This guide is the operator runbook for using any Homeboy-managed controller
machine as a broker while any configured runner machine claims work by making an
outbound connection to that controller. A controller can be a VPS, a workstation,
or another server; a runner can be a lab box, build host, desktop, or any machine
with a Homeboy runner record. Extra Chill VPS to `homeboy-lab` is only the first
concrete deployment profile for this path. The broker, worker, pairing,
and workspace sync model is reusable. It tracks the end-to-end shape from issue
#2993 and the parent epic #2950.

## Status

The current main branch contains the reverse-runner broker/job substrate and
controller-side runner policy commands, but the production setup is gated by
follow-up work:

| Area | Status | Gate |
|---|---|---|
| Broker authentication and pairing | Not production-safe yet | #2990 |
| Long-running runner worker service | Not available as a durable service yet | #2991 |
| Controller broker service deployment | Not available as a packaged service recipe yet | #2992 |
| Reverse workspace sync | Not available for controller-to-runner workspaces without direct SSH | #2947 |

Until those issues land, use this guide as scaffolding for local/private smoke
tests and operator preparation. Do not expose the broker publicly or rely on this
path for production Lab offload of portable commands.

## Topology

```text
Controller machine
  homeboy daemon / reverse broker
  runner trust policy
  job submission
        ^
        | HTTPS or private tunnel, authenticated after #2990
        |
Runner machine
  runner pairing policy
  reverse worker service after #2991
  workspace materialization after #2947
```

The runner initiates all broker traffic. The controller does not need SSH access
to the runner for the reverse path, and the runner should not be opened to the
public internet.

## Example profile: Extra Chill

The examples below use placeholders so the same runbook applies to any VPS. For
the current Extra Chill deployment, use these values:

| Placeholder | Extra Chill value |
|---|---|
| `<project-id>` | `extrachill` |
| `<controller-server-id>` | `extra-chill` |
| `<controller-label>` | `extra-chill-vps` |
| `<runner-id>` | `homeboy-lab` |
| `<runner-workspace-root>` | `/home/user/Developer` |
| `<runner-user>` | `deploy` |

## 1. Controller broker setup

Run this on the controller machine.

1. Confirm Homeboy can see the project and controller server record:

   ```sh
   homeboy project show <project-id> --output /tmp/homeboy-controller-project.json
   homeboy server show <controller-server-id> --output /tmp/homeboy-controller-server.json
   ```

2. Enable a broker-capable daemon in a private or protected network context.
   The exact service wrapper and safe public binding are gated by #2992 and
   broker auth is gated by #2990. Until those land, keep the daemon loopback-only
   or behind a private tunnel:

   ```sh
   homeboy daemon start --addr 127.0.0.1:0 --output /tmp/homeboy-reverse-broker.json
   ```

3. Record the broker URL label operators will use in evidence. For private
   smoke tests this can be a tunnel label rather than a public URL:

   ```sh
   export HOMEBOY_RUNNER_BROKER_LABEL=<controller-label>
   export HOMEBOY_RUNNER_BROKER_URL=https://broker.example.invalid
   ```

4. Trust the runner for only the needed project, command families, and
   workspace roots. This records policy on the controller side; secure token
   material and pairing enforcement are completed by #2990:

   ```sh
   homeboy runner trust <runner-id> \
     --peer <controller-server-id> \
     --project <project-id> \
     --command runner.exec \
     --command audit \
     --command lint \
     --command test \
     --command bench \
     --command trace \
     --workspace-root <runner-workspace-root> \
     --allow-raw-exec false \
     --artifact-policy metadata
   ```

5. After #2992 lands, install the broker as a service and place it behind the
   documented TLS/private tunnel/auth boundary. The service should persist job
   store state, expose health/status, and make restart behavior explicit for
   active claims.

## 2. Runner pairing and auth

Run this on the runner machine.

1. Confirm the runner record exists and has a confined workspace root:

   ```sh
   homeboy runner show <runner-id> --output /tmp/homeboy-runner.json
   homeboy runner doctor <runner-id> --path <runner-workspace-root>/homeboy --extension rust
   ```

2. Pair the runner with the controller policy. This is the
   runner-side counterpart to `runner trust`; #2990 supplies the secured token
   and enforcement model:

   ```sh
   homeboy runner pair <runner-id> \
     --peer <controller-server-id> \
     --accept-project <project-id> \
     --workspace-root <runner-workspace-root> \
     --allow-raw-exec false
   ```

3. Store any future broker tokens through the Homeboy auth/secrets path described
   by #2990. Do not put broker tokens in shell history, systemd unit files, or
   normal command output.

## 3. Runner worker service setup

The durable worker service is gated by #2991. The intended shape is one command
on the runner that continuously claims jobs for `<runner-id>`, applies runner
policy, executes allowed work, streams events, and finishes each job.

Expected service command after #2991:

```sh
homeboy runner work <runner-id> \
  --broker-url "$HOMEBOY_RUNNER_BROKER_URL" \
  --loop \
  --output /var/tmp/homeboy-reverse-worker-start.json
```

Expected systemd outline after #2991:

```ini
[Unit]
Description=Homeboy reverse runner worker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=<runner-user>
Environment=HOMEBOY_RUNNER_BROKER_URL=https://broker.example.invalid
ExecStart=/usr/local/bin/homeboy runner work <runner-id> --broker-url ${HOMEBOY_RUNNER_BROKER_URL} --loop --output /var/tmp/homeboy-reverse-worker.json
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Keep the service disabled for production until #2990 and #2991 are merged. For
private smoke tests before #2991, use the one-shot worker shape from the active
implementation branch or PR notes instead of a hand-rolled infinite loop.

## 4. Reverse workspace sync

Reverse workspace sync is gated by #2947. Until it lands, controller-originated
commands can only smoke simple brokered execution that does not require syncing a
controller checkout to the runner through the reverse session.

Expected modes after #2947:

- `git`: the runner materializes a repo-backed clean commit/ref itself.
- `snapshot-over-tunnel`: the controller streams a filtered archive through the reverse
  session.
- `patch-over-tunnel`: the controller sends a clean base plus a diff for uncommitted
  work.

All modes must keep workspace paths under the configured runner workspace root,
preserve snapshot provenance, and apply the existing secret/path exclusions used
by `homeboy runner workspace sync`.

## 5. Minimal end-to-end smoke

Run this only after #2990, #2991, #2992, and #2947 have landed or in an explicit
private smoke environment that provides equivalent branch builds.

1. Start or verify the controller broker service:

   ```sh
   homeboy runner status <runner-id> --output /tmp/homeboy-runner-status.json
   ```

2. Start or verify the runner worker service:

   ```sh
   systemctl --user status homeboy-reverse-runner.service
   journalctl --user -u homeboy-reverse-runner.service -n 50 --no-pager
   ```

3. Submit a minimal command from the controller:

   ```sh
   homeboy runner exec <runner-id> \
     --project <project-id> \
     --cwd <runner-workspace-root> \
     --output /tmp/homeboy-runner-smoke.json \
     -- /bin/sh -lc 'printf "homeboy-runner-smoke\\n"'
   ```

4. Expected evidence shape:

   ```json
   {
     "command": "runner.exec",
     "runner_id": "<runner-id>",
     "mode": "reverse_broker",
     "transport": "reverse_broker",
     "broker_label": "<controller-label>",
     "job_id": "...",
     "exit_code": 0,
     "stdout_sample": "homeboy-runner-smoke"
   }
   ```

For PR or incident evidence, capture the command output plus the matching worker
log lines showing the same `job_id`, claim, event, and finish result.

The controller broker API wraps every successful response as
`success: true` with endpoint data under `data.body`. Consumers should treat
responses without `data.body` as malformed instead of falling back to direct
`data` payload parsing.

## Troubleshooting

| Symptom | Check | Likely fix |
|---|---|---|
| Auth failure | Broker returns a structured auth or forbidden error. | Re-pair the runner/controller after #2990, rotate token, and confirm runner ID/project policy match. |
| No jobs claimed | Worker logs show empty claims or no claim attempts. | Confirm broker URL, runner ID, worker service status, and that submitted jobs target the expected runner. |
| Stale claims | Job remains claimed with no finish event. | Restart the worker, inspect broker job status, and use the #2992 recovery command once available. |
| Worker offline | `runner status` or service status shows no active worker heartbeat. | Restart the runner worker service and inspect journald for broker/network/auth errors. |
| Workspace sync failure | Error names transport, materialization, policy, or command execution. | Use `git` mode for clean repo-backed work, or wait for #2947 for reverse snapshot/patch transport. |
| Policy denial | `runner.exec` reports project, command, raw exec, or workspace-root denial. | Update `runner trust` on the controller and `runner pair` on the runner with the narrow missing permission. |

## Cleanup and restart

Use the narrowest cleanup that matches the failure.

1. Stop the runner worker:

   ```sh
   systemctl --user stop homeboy-reverse-runner.service
   ```

2. Disconnect stale local runner session metadata when direct or reverse session
   metadata is wrong:

   ```sh
   homeboy runner disconnect <runner-id>
   ```

3. Restart the controller broker service after #2992 supplies the packaged unit:

   ```sh
   systemctl restart homeboy-reverse-broker.service
   ```

4. Start the runner worker again:

   ```sh
   systemctl --user start homeboy-reverse-runner.service
   ```

5. Re-run the minimal smoke and compare `job_id`, `runner_id`, `mode`,
   `exit_code`, and stdout evidence before enabling automatic hot-command
   offload.
