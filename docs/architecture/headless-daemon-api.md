# Headless Daemon API Contract

Homeboy is CLI-first, but the daemon is the stable local UI and automation
surface for clients that should not shell out and parse terminal output. The
daemon remains a local Homeboy engine, not a hosted control plane.

For reverse runner work, a VPS can run the daemon as a broker service while
still keeping the daemon itself on loopback. Use `homeboy daemon broker-config`
to render the `systemd` unit and private tunnel/proxy scaffolding. Public broker
exposure remains blocked until the broker auth/pairing work in
[#2990](https://github.com/Extra-Chill/homeboy/issues/2990) lands.

## Scope

The daemon owns a loopback-only HTTP contract for:

- local client discovery and health checks
- read-only component, rig, stack, run, artifact, and finding inspection
- long-running lint, test, audit, and bench jobs
- typed, allowlisted sandbox-agent Homeboy tool jobs
- structured job events and final results
- future mutating operations behind explicit capabilities and confirmations

Reverse runner broker routes add a private controller/runner job exchange:

- `POST /runner/sessions` for runner-initiated session registration
- `POST /runner/jobs` for the controller to enqueue work
- `POST /runner/jobs/claim` for a reverse runner to claim queued work
- `POST /runner/jobs/:id/events` for runner progress events
- `POST /runner/jobs/:id/finish` for terminal runner results

These routes are not safe as an unauthenticated public service. Until #2990 is
available, deploy them only on loopback plus private SSH/VPN/Zero Trust tunnel
access.

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

Reverse runners invert that flow. The controller/VPS runs `homeboy daemon serve`
as a loopback broker, and the lab connects out to a private broker URL so the lab
does not need inbound ports:

```text
VPS systemd service
  homeboy daemon serve --addr 127.0.0.1:7421
          |
          +-- /var/lib/homeboy/.config/homeboy/daemon/state.json
          +-- /var/lib/homeboy/.config/homeboy/daemon/jobs.json
          |
private tunnel / private network only
          |
Runner machine
  POST /runner/sessions
  POST /runner/jobs/claim
  POST /runner/jobs/:id/events
  POST /runner/jobs/:id/finish
```

Use `homeboy daemon broker-config` on the target VPS to render the
service unit, proxy snippets, safe exposure state, and operational commands.

## Minimum Dashboard Surface

A useful headless UI can be built from this read/query surface:

- `GET /health` and `GET /version` for connectivity and compatibility
- `GET /config/paths` for local configuration discovery
- `GET /components`, `GET /components/:id`, `GET /components/:id/status`, and
  `GET /components/:id/changes` for source checkout selection
- `GET /rigs`, `GET /rigs/:id`, and `POST /rigs/:id/check` for environment
  readiness
- `GET /stacks`, `GET /stacks/:id`, and `POST /stacks/:id/status` for stacked
  branch inspection
- `GET /runs`, `GET /runs/:id`, `GET /runs/:id/artifacts`,
  `GET /runs/:id/artifacts/sync`, `GET /runs/:id/artifacts/:artifact_id`,
  `GET /runs/:id/artifacts/:artifact_id/content`, and `GET /runs/:id/findings`
  for persisted evidence
- `GET /audit/runs` and `GET /bench/runs` for analysis-specific run history
- `GET /jobs`, `GET /jobs/:id`, `GET /jobs/:id/events`, and
  `POST /jobs/:id/cancel` for long-running work
- `GET /tools`, `GET /tools/:id`, and `POST /tools/:id/run` for sandbox agents
  that need typed Homeboy tool execution without arbitrary shell access

This is enough for a dashboard that lists components, shows selected checkout
state, displays rigs/stacks, starts analysis jobs, streams progress, and renders
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
POST /lint|/test|/audit|/bench
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

- `homeboy.audit` with `run:audit`
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

`POST /exec` remains a daemon-internal structured execution route for existing
runner plumbing. It is not the sandbox-agent contract.

The packaged broker service sets `HOME=/var/lib/homeboy`, so the daemon durable
job store is `/var/lib/homeboy/.config/homeboy/daemon/jobs.json`. The store keeps
bounded per-job events and supports restart recovery for queued broker jobs. It
is operational state, not a durable audit archive; important evidence should
still be persisted as Homeboy observations or artifacts.

Restart behavior:

- queued remote-runner jobs stay queued across daemon restart
- broker-owned running jobs are marked failed as stale when the store is reopened
- active reverse-runner claims remain lease-scoped until expiry
- runners should retry claim after lease expiry when the broker restarts mid-job

## Client Replacement Order

Headless clients should replace shell-out flows in low-risk order:

1. Discovery: `daemon status`, `health`, and `version`.
2. Dashboard reads: components, status, changes, rigs, stacks, runs, artifacts,
   and findings.
3. Analysis jobs: lint, test, audit, and bench with event polling.
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
- `run:audit`
- `run:bench`
- `write:files`
- `write:git`
- `write:rig`
- `write:stack`
- `operator:ssh`
- `operator:deploy`
- `operator:release`

Risk categories:

- Read-only: status, inventory, persisted runs, artifacts, findings.
- Bounded local run: lint, test, audit, bench, rig check, stack status.
- Local write: file edits, refactor fixes, generated patches.
- Git write: commit, tag, push, stack apply/sync/push.
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
- Keep VPS broker services on a stable loopback port and expose them only through private tunnels until broker auth/pairing lands.
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
