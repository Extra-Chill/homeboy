# `homeboy daemon`

Run and inspect the local-only Homeboy HTTP API daemon.

## Synopsis

```sh
homeboy daemon <COMMAND>
```

## Subcommands

- `start` — start the local daemon in the background
- `serve` — run the daemon in the foreground
- `stop` — stop the background daemon recorded in the state file
- `status` — show daemon state and selected local address
- `broker-config` — render a deployable reverse-runner broker service recipe

## Local HTTP API

The daemon binds to loopback only. `homeboy daemon start` writes the selected
address and PID to the daemon state file so headless clients can discover it via
`homeboy daemon status`.

Always treat the API as a local UI contract. It is not a hosted or remote
multi-user service.

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
