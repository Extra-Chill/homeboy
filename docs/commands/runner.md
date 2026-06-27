# `homeboy runner`

## Synopsis

```sh
homeboy runner <COMMAND>
```

`runner` manages durable execution backends. SSH runners are a capability on a `homeboy server` record, so the common Lab flow uses one ID for the machine and its runner. Local runners remain standalone because they describe this machine rather than an SSH server. Both storage shapes share the same runner capability contract: workspace root, settings, environment, secret references, resources, and policy.

Runner configuration separates printable environment from secrets:

- `env` is for non-secret values that are useful in diagnostics, such as `HOMEBOY_PUBLIC_ARTIFACT_BASE_URL`.
- `secret_env` is for execution-time secret references like `{ "env": "NAME" }` or `{ "file": "~/.config/homeboy/secrets/name" }`.
- Command output redacts sensitive names in `env` and prints only `secret_env` references, never resolved secret values.

## Subcommands

### `add`

```sh
homeboy runner add <id> --workspace-root <path>
homeboy runner add <server-id> --server <server-id> --workspace-root <path>
homeboy runner add --json <spec>
```

Options:

- `--kind local|ssh`: explicit runner kind. Defaults to `ssh` when `--server` is set, otherwise `local`.
- `--server <server-id>`: existing `homeboy server` record for SSH runners. For SSH runners, `<id>` must match `<server-id>`.
- `--workspace-root <path>`: workspace root on the runner machine.
- `--homeboy-path <path>`: Homeboy binary path on the runner machine.
- `--daemon`: marks the runner as daemon-preferred for future commands.
- `--concurrency-limit <n>`: maximum concurrent workflows this runner should accept.
- `--artifact-policy <label>`: artifact policy label reserved for future execution commands.

### `enable`

```sh
homeboy runner enable <server-id> --workspace-root <path>
homeboy runner enable <server-id> --workspace-root <path> --concurrency-limit 4 --artifact-policy copy
```

Enables runner capability on an existing SSH server. This is the recommended onboarding path for any machine that should accept Homeboy runner work:

```sh
homeboy server create <runner-id> --host <host> --user <user> --port 22
homeboy runner enable <runner-id> --workspace-root <workspace-root> --concurrency-limit 4 --artifact-policy copy
homeboy runner connect <runner-id>
```

After this, `<runner-id>` is both the server ID and the runner ID.

Commands that are both resource-policy hot and portable for Lab offload (`agent-task controller from-spec --resume`, `agent-task controller run-from-spec`, `agent-task controller materialize`, `audit`, full `lint`, `test`, `bench run`, `fuzz run`, and `trace`) auto-select a default runner when `--runner` is omitted. Selection is conservative:

- `--runner <id>` always wins.
- `--force-hot` only suppresses the resource-policy warning. If a default Lab runner is available for a portable hot command, Homeboy refuses to use `--force-hot` as an implicit local bypass.
- `--force-hot --allow-local-hot` keeps a portable hot command local even when a default Lab runner is available, unless a command-specific host policy denies local execution. For benchmarks, `homeboy config set /bench/local_execution '"denied"'` makes local `homeboy bench` execution fail closed until the global config is changed back.
- `lab.preferred_runner` is used when it names an SSH runner, even if that runner is not connected yet.
- Without `lab.preferred_runner`, Homeboy auto-selects only when exactly one SSH runner is configured or exactly one SSH runner is already connected.
- With a preferred or uniquely configured Lab runner, `homeboy bench <component>` routes to Lab directly; `--runner <id>` is only needed to override an ambiguous or non-default runner selection.
- Local runners are never auto-selected.
- If the auto-selected runner is disconnected, Homeboy attempts a short bounded `runner connect` before execution. Connection failure prints the reason and falls back to local execution.
- Explicit `--runner <id>` also attempts to connect a disconnected runner, but connection failure remains a command error instead of falling back silently.

Observation metadata records the routing decision under `metadata.lab_offload` when an observed run is created. The stable contract is `schema: "homeboy/lab-offload/v1"` and keeps the existing top-level compatibility fields: `source` is `automatic` or `explicit`; `status` is `offloaded`, `skipped`, or `fallback`; successful offloads include `runner_id` plus `remote_workspace`; local fallback records the runner and `fallback_reason`; skipped local execution records why no automatic offload was used, such as `force_hot`, `force_hot_local_override`, or `no_default_runner`. The same object also carries `plan_id` and plan-derived phase fields including `sync_mode`, `capability_preflight`, `extension_parity`, and `patch_captured`.

Commands launched through non-local `homeboy runner exec` run with `HOMEBOY_RUNNER_HOSTED_EXEC=1`. That marker is Homeboy's first-class runner-side dispatch signal: nested runner commands such as `homeboy agent-task cook` are allowed to pass the non-interactive resource preflight without adding `--force-hot`, because the work is already intentionally hosted on the selected runner.

For reviewer-facing output, treat `runner exec` stdout as operator context and
record durable artifacts through the run/artifact loop. Pass `--run-id <id>` plus
`--artifact <path>` for files/directories and `--summary <path>` for compact
summary evidence produced by the runner command. JSON summary files are echoed in
the structured `runner.exec` response under `structured_summaries`, with the run
id, runner id, command, declared path, and promoted artifact id/path so LLM
workflows can consume the compact result without spelunking remote stdout or
artifact directories. See
[Artifact loop for runner and matrix workflows](../operators/artifact-loop-runner-matrix.md)
for static HTML and matrix examples, plus proposed attach/promote command shapes
for successful runner jobs that produced files but registered zero artifacts.

Lab offload support is intentionally command-specific:

| Command | Auto offload | Explicit `--runner` | Decision |
|---|---:|---:|---|
| `agent-task cook` / `agent-task run-plan` | Yes | Yes | Portable agent-task execution when the command has deterministic gates where required. |
| `agent-task controller from-spec --resume` / `agent-task controller run-from-spec` / `agent-task controller materialize` | Yes | Yes | Portable controller spec materialization defaults to Lab when a default runner exists. |
| `agent-task controller resume` | No | Yes | Runner-resident controller lifecycle work. |
| `agent-task retry --run` | Yes | Yes | Retry plus execution follows the portable agent-task run path. |
| `agent-task run` / `run-next` / `status` / `logs` / `artifacts` / `review` / `providers` | No | Yes | Runner-resident inspection and queue interaction. |
| `agent-task auth status` | No | Yes | Runner-resident auth diagnostics. |
| `audit` full workspace | Yes | Yes | Safe single-workspace replay after snapshot sync. |
| `audit --changed-since` | No | No | Runs locally for now because changed-since audit depends on git base refs that Lab sync may not have fetched. The Lab plan records the skipped local-only decision. |
| `bench run` / default bench run | Yes | Yes | Safe single-workspace replay; local baseline/ratchet writes are treated as mutation flags. |
| `build` | No | No | Local quality gate; extension-owned build commands are not represented in the portable Lab contract yet. |
| `fuzz run` / default fuzz run | Yes | Yes | Safe single-workspace replay with fuzz-specific case/result artifacts. |
| `lint` full workspace | Yes | Yes | Safe single-workspace replay; `--fix` is treated as a mutation flag. |
| `lint --changed-since` / `lint --changed-only` | No | No | Runs locally for now because changed-file scopes are not represented in the Lab portability contract yet. The Lab plan records the skipped local-only decision. |
| `refactor --from audit` / `refactor --all` | No | Yes | Source refactor runs with command-owned mutation flags and runner-side workspace controls. |
| `review` | Yes | Yes | Scoped audit/lint/test umbrella using the same portable quality gates. |
| `rig check` | No | Yes | Runner-side rig validation without starting local services. |
| `test` full workspace | Yes | Yes | Safe single-workspace replay with runner extension parity preflight. |
| `test --changed-since` | No | No | Runs locally for now because changed-since test selection depends on git base refs that Lab sync may not have fetched. The Lab plan records the skipped local-only decision. |
| `trace` | Yes | Yes | Safe single-workspace replay with Playwright/browser capability gate. |
| `tunnel preview-consumer run` | No | Yes | Runner-hosted preview consumer execution. |
| `tunnel service expose` / `tunnel service start` | No | Yes | Runner-hosted private service tunnel lifecycle. |
| `rig up` | No | No | Stays local because rig pipelines manage local services, leases, ports, and declared filesystem paths that the current single-workspace snapshot cannot safely mirror. |
| `fleet exec` | No | No | Stays local because fleet execution depends on local fleet/project/server config before opening SSH sessions to each project; runner-side config parity is not guaranteed. |

Local-only resource-pressure commands still get resource-policy warnings, but those warnings explain why Lab offload is unavailable instead of suggesting `--runner`.

Configure a preferred runner with:

```sh
homeboy config set /lab/preferred_runner '"<runner-id>"'
```

### `doctor`

```sh
homeboy runner doctor local
homeboy runner doctor <runner-id>
homeboy runner doctor <runner-id> --path <component-path> --extension rust
homeboy runner doctor <runner-id> --require-tool zip --require-tool unzip
homeboy runner doctor <runner-id> --scope lab-offload
homeboy runner doctor <runner-id> --scope lab-offload --repair
```

Diagnoses a local or configured SSH runner without mutating it. Use `local`,
`localhost`, or `self` to inspect this machine without creating a runner record.
The JSON payload uses `command: "runner.doctor"` and includes `runner_id`,
`status`, `capabilities`, and warning/error details when a capability probe fails.

Use `doctor` before `connect` when you need to know whether Homeboy, Git, SSH,
and the configured workspace root are usable on the target machine.

Safety manifest metadata marks `runner connect` and `runner work` as explicit
operator lifecycle actions. They do not currently expose dry-run contracts; use
`runner doctor` for preflight diagnostics before changing runner lifecycle state.

Use `--scope lab-offload` before serious Lab evidence runs. It adds checks for
the configured runner Homeboy command, bare `homeboy` PATH resolution, preferred
runner binaries, connected daemon exec readiness, and Sample Runtime runner-path
freshness signals. When Homeboy can identify a safe exact recovery command, the
check includes that remediation instead of leaving the operator to infer it from
logs.

Use `--scope lab-offload --repair` for the narrow self-healing path. Today this
reconnects a failing direct Lab runner daemon and reruns the daemon exec probe.
It does not upgrade binaries, rewrite runner paths, or refresh Sample Runtime caches;
those remain explicit operator actions because they can be expensive or depend
on environment-specific paths.

`runner status` includes generic `selected_lab_runner.executable_requirements`
from declared agent runtime manifests. Each entry names the runtime, requirement
id, non-secret env vars, candidate executable names/paths, optional version
arguments, and install hint. Homeboy owns the generic semantics: runtimes declare
the executable they need; runner diagnostics expose the declaration without
embedding product-specific checks in core.

`runner status` also includes `selected_lab_runner.wp_codebox_runtime` for the
legacy runner job environment projection. That block shows the configured WP
Codebox CLI, managed cache source/binary, expected
`@automattic/wp-codebox-playground` and `@automattic/wp-codebox-core` paths, a
runner-side probe command that prints the exact effective runtime and git SHA,
and warnings when configured paths mix a stale checkout with the managed cache.

### `refresh-homeboy`

```sh
homeboy runner refresh-homeboy <runner-id> --ref main --reconnect
homeboy runner refresh-homeboy <runner-id> --source https://github.com/Extra-Chill/homeboy.git --ref <branch-or-sha>
homeboy runner refresh-homeboy <runner-id> --select /path/to/homeboy --reconnect
homeboy runner refresh-homeboy <runner-id> --ref <branch-or-sha> --dry-run
```

Builds or selects the Homeboy binary used by runner/Lab job execution without
mutating a runner's primary checkout. Materialize mode clones/fetches the
Homeboy source into a managed runner-side cache under the runner workspace root
unless `--target-dir` is supplied, hard-resets that isolated checkout to the
requested ref, builds `target/release/homeboy`, probes `self identity`, and then
updates the runner `homeboy_path` to the clean binary. Select mode skips the
build and probes the exact binary passed to `--select` before updating
`homeboy_path`.

Use `--reconnect` when the active daemon/session should be refreshed immediately.
Without it, the JSON output includes follow-up `disconnect`, `connect`, and
`status` commands so operators can restart a session at the right time. `runner
status` also reports controller, configured executable, active daemon version,
build identity, drift signals, and refresh commands under
`selected_lab_runner.runner_homeboy`.

Pass one or more `--require-tool <command>` values when a provider or job path
knows it needs additional runner-side commands before starting expensive work.
Doctor resolves each command on the runner `PATH` and reports missing requested
tools as `tool.required.<command>` errors with install/setup remediation. This is
generic: provider layers declare their own tools; Homeboy core only checks command
availability.

Programmatic runner execution can use the same generic boundary through
`RunnerCapabilityPreflight.required_commands`. Those commands are checked before
remote execution starts, alongside existing required tools, components, and
environment variables.

#### Runner-managed dependency sources

Portable Lab/evidence runs should declare runner dependency sources instead of
smuggling local checkout paths through workflow-specific environment variables.
Homeboy's generic contract is the extension/component setting
`validation_dependencies`: each entry is a component id or explicit checkout
path that the runner workspace sync treats as managed source input.

For each declared dependency, `homeboy runner workspace sync`:

- resolves a sibling checkout, registered component, or deterministic clone;
- rejects dirty, stale, divergent, missing-upstream, or ambiguous Git state;
- runs the dependency install/build lifecycle in a prepared copy;
- materializes the prepared dependency beside the primary runner workspace; and
The JSON output includes `validation_dependencies`, with each dependency's
`id`, `role`, controller `local_path`, runner `remote_path`, and
`evidence_path`. Bench, trace, eval, and provider layers can consume that
generic output to populate their own path settings without knowing Lab
filesystem layouts or dependency-specific environment variable names.

Example manifest fragment:

```json
{
  "extensions": {
    "provider-id": {
      "settings": {
        "validation_dependencies": ["runtime-component"]
      }
    }
  }
}
```

Pass one or more `--extension <id>` values to validate extension parity before
Lab offload. Doctor runs the same `homeboy extension show <id>` contract on the
target runner that test offload uses at execution time. `--path` sets the probe
working directory when the extension should resolve from a specific component
checkout. Missing extensions are reported as `extension.parity` errors with an
install command such as `homeboy extension install <source> --id rust`.

Lab offload for portable resource-pressure commands uses the same capability vocabulary before running on
an explicit `--runner`. Homeboy currently gates `lint`, `test`, `audit`,
`bench`, and `trace` against the source worktree's lightweight tool signals:

- `package.json` requires `node` and `npm`.
- `pnpm-lock.yaml` requires `node` and `pnpm`.
- `composer.json` requires `php` and `composer`.
- Docker/Compose files require `docker`.
- `trace` requires Playwright plus browser binaries.

When an explicit runner is missing required tools, the command fails before
workspace sync with a `runner_capabilities` validation error and remediation.
The same central policy returns a local fallback reason for future automatic
Lab offload selection.

### `connect`

```sh
homeboy runner connect <runner-id>
homeboy runner connect <controller-id> --reverse --reverse-runner <runner-id> --broker-url <url>
```

Starts a loopback-only Homeboy daemon on the runner and opens an SSH tunnel to
it. This is the preferred Lab execution path because later `runner exec` calls
can use the daemon session instead of ad-hoc SSH command execution. The JSON
payload uses `command: "runner.connect"` and reports connection state such as
the runner ID, tunnel endpoint, daemon endpoint, and persisted session metadata.

Reverse runner connections record the runner-initiated session substrate and use
the controller daemon as the broker. A reverse runner can register itself with
`POST /runner/sessions`; the controller then reports that runner as connected
and routes `runner exec` through brokered jobs instead of a direct daemon URL.
The broker exposes `POST /runner/jobs`, `POST /runner/jobs/claim`,
`POST /runner/jobs/<job-id>/events`, and `POST /runner/jobs/<job-id>/finish` so
controllers can queue work and reverse runners can claim, stream progress, and
return results without inbound access to the lab machine.

Use Homeboy wrappers for broker maintenance and artifact lookup instead of raw
HTTP calls:

```sh
homeboy runner job reconcile <runner-id>
homeboy runner job artifacts <runner-id> <job-id> <artifact-id>
```

#### Broker authentication and pairing

Every `/runner/*` broker route requires an authenticated, scoped bearer token
(#2990). The trust model is:

- **Pairing.** On the broker host, mint a credential with
  `homeboy runner broker pair <id> --runner-id <runner-id> [--work] [--submit]`.
  Pairing prints a one-time bearer token and stores only its SHA-256 hash in
  `~/.config/homeboy/broker_auth.json` (`0600`). The plaintext token is shown
  once and never re-displayed or logged.
- **Scopes.** `--work` authorizes worker routes (register, claim, events,
  finish, heartbeat); `--submit` authorizes controller job submission and
  cancellation (`POST /runner/jobs`, `POST /runner/jobs/<id>/cancel`). A
  credential may carry both. When neither flag is given, `--work` is assumed.
- **Runner-id binding.** Worker routes carry a `runner_id`; the presented token
  must belong to a credential paired to that exact `runner_id`, so a paired
  runner can never claim, progress, or finish another runner's jobs.
- **Revocation.** `homeboy runner broker revoke <id>` disables a credential.
  `homeboy runner broker list` shows non-secret credential metadata (never
  tokens or hashes).
- **Secure by default.** With no credentials configured, the broker rejects all
  `/runner/*` traffic with a structured `broker.auth_denied` (`401`) error. To
  keep an existing loopback-only smoke setup working without auth, set
  `"allow_unauthenticated_loopback": true` in `broker_auth.json`; this is only
  honored when the broker is bound to loopback.

Workers send the token via `--broker-token <token>` or the
`HOMEBOY_BROKER_TOKEN` environment variable; controllers read their submit token
from `HOMEBOY_BROKER_TOKEN`. Tokens travel as both `X-Homeboy-Broker-Token` and
`Authorization: Bearer <token>` so they survive proxies that strip one form.
Unauthenticated, mismatched-runner, or wrong-scope requests are rejected with a
structured `broker.auth_denied` error and never expose the stored secret.

Daemon and broker HTTP responses use one canonical envelope. The outer response
reports transport success and the endpoint payload always lives under
`data.body`; runner clients require that shape and do not parse legacy direct
`data` payloads.

Runner lifecycle output is transport-neutral even when the underlying route is a
direct daemon tunnel or a reverse broker. JSON consumers should prefer these
generic contracts:

- `RunnerSession`: persisted connection/session ownership, including mode, role,
  controller/broker references, process IDs, version identity, and heartbeat.
- `RunnerJob`: active or inspected job state with lifecycle owner, claim lease,
  command, status, durable run linkage, and artifact refs.
- `RunnerWorkspaceLease`: controller-owned local-to-runner workspace materialization
  with source ref/commit/dirty metadata.
- `RunnerWorkspaceLeaseSet`: named workspace leases for jobs that materialize more
  than one tree. `primary` names the default execution workspace; entries in
  `leases` bind names such as `source`, `rig`, or `runtime` to their own
  `RunnerWorkspaceLease`. Existing single-workspace output keeps
  `workspace_lease` while migrations add `workspace_leases`.
- `RunnerResult`: terminal command result with exit status, stream sizes, mirrored
  run linkage, and artifact refs.
- `RunnerArtifactRef`: stable artifact identity plus path/URL/hash/transport
  metadata for later retrieval.
- `RunnerMutationArtifacts`: transport-independent mutation return shape. When a
  runner command captures source changes, `mutation_artifacts.patch_ref` points
  at the patch artifact; future file-bundle and operation-log returns use
  `file_bundle_ref` and `operation_log_ref` without changing caller logic.
- `RunnerHandoff`: the controller-to-runner handoff envelope tying transport,
  owner, job, workspace lease, and result together.

Compatibility fields such as `active_jobs`, `job`, `job_id`, `job_events`, and
`workspace_lease` remain present for existing callers. New integrations should
read `active_runner_jobs`, `runner_job`, `workspace_leases`, `runner_result`,
`mutation_artifacts`, `artifact_ref`, and `handoff` so direct SSH and reverse
broker implementations stay hidden behind the same lifecycle vocabulary.

Connected daemon status also reports stale runtime-path signals. When the daemon
started with configured `HOMEBOY_*_COMPONENT_PATH`, `HOMEBOY_*_PLUGIN_PATH`,
`HOMEBOY_*_PROVIDER_PATH`, or `HOMEBOY_*_RUNTIME_PATH` values, `/version`
captures start-time fingerprints and compares them with the current on-disk
paths. `homeboy runner status <runner-id>` surfaces those differences in
`stale_daemon.stale_runtime_paths`, and surfaces runner config path changes in
`stale_daemon.changed_runtime_paths`. This is intentionally read-only: refresh a
development runner with `homeboy runner disconnect <runner-id>` followed by
`homeboy runner connect <runner-id>` after rebuilding runner-side runtime code.

```json
{
  "success": true,
  "data": {
    "status": 200,
    "endpoint": "runner.jobs.submit",
    "body": {
      "job": {},
      "poll": {}
    }
  }
}
```

For the generic controller-to-runner operator path, see
[Controller to runner reverse-runner setup](../operators/controller-runner-reverse-runner.md).
That guide is machine-agnostic and intentionally explicit about what is available
now and what remains gated by #2990, #2991, #2992, and #2947 before production
broker exposure.

### `work`

```sh
homeboy runner work <runner-id> --broker-url <url> --broker-token <token>
homeboy runner work <runner-id> --broker-url <url> --project <project-id> --lease-ms 30000
HOMEBOY_BROKER_TOKEN=<token> homeboy runner work <runner-id> --broker-url <url> --loop
```

Pass the paired token with `--broker-token` or the `HOMEBOY_BROKER_TOKEN`
environment variable. The token is required whenever the broker enforces auth;
omit it only for a loopback-open smoke broker.

Claims one brokered reverse-runner job for the runner, executes it on the runner
machine under the runner's local policy, streams a progress event, and finishes
the broker job with stdout, stderr, and exit code. This is the runner-side half
of reverse `runner exec`; it uses outbound HTTP from the lab to the controller
broker and does not require inbound SSH or a public listening port on the lab.

The command exits `0` when no job is available, with `claimed: false` in the JSON
payload. When a job is claimed, the process exit code matches the executed
command's exit code.

Use `--loop` for a long-running reverse runner service. Loop mode emits one
structured JSON status line per lifecycle event to stderr so systemd/journald can
index startup, idle backoff, job completion, transient broker failures, and
shutdown without mixing those events into the final stdout JSON payload. Empty
queues use exponential backoff controlled by `--idle-backoff-ms` and
`--max-idle-backoff-ms`, so workers do not hot-spin when no work is available.
Transient broker failures sleep for `--broker-failure-backoff-ms` and exit
non-zero after `--broker-retry-limit` consecutive failures. `SIGINT` and
`SIGTERM` request graceful shutdown after the current claim attempt or job.

### `broker`

```sh
homeboy runner broker pair lab-1 --runner-id homeboy-lab --work
homeboy runner broker pair ctrl-1 --runner-id homeboy-lab --submit
homeboy runner broker revoke lab-1
homeboy runner broker list
```

Manage reverse runner broker authentication and pairing on the broker host.
`pair` mints a one-time scoped bearer token (printed once; only its SHA-256 hash
is stored under `~/.config/homeboy/broker_auth.json`). `revoke` disables a
credential by id, and `list` reports non-secret credential metadata. See
[Broker authentication and pairing](#broker-authentication-and-pairing) for the
full trust model.

### `job`

```sh
homeboy runner job logs <runner-id> <job-id>
homeboy runner job logs <runner-id> <job-id> --follow --poll-ms 1000
homeboy runner job cancel <runner-id> <job-id>
```

Inspects or cancels durable runner daemon jobs after `runner exec` or Lab offload
has submitted work to a connected runner. `logs` fetches the persisted job plus
its event stream; `--follow` keeps polling until the job reaches a terminal state
and prints newly observed events as they arrive. Use this when a controller exits
after dispatching runner work and you need to inspect the already-started job.

`cancel` requests cancellation for a queued or running durable runner daemon job
through the connected runner daemon.

Minimal Homeboy Lab systemd unit:

```ini
[Unit]
Description=Homeboy reverse runner worker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=deploy
WorkingDirectory=/home/user
Environment=HOMEBOY_BROKER_TOKEN=<paired-token>
ExecStart=/usr/local/bin/homeboy runner work homeboy-lab --broker-url https://controller.example.com --loop --idle-backoff-ms 1000 --max-idle-backoff-ms 30000 --broker-retry-limit 12
Restart=on-failure
RestartSec=10
KillSignal=SIGTERM

[Install]
WantedBy=multi-user.target
```

The worker authenticates with the broker using the paired bearer token. Prefer
delivering it through `HOMEBOY_BROKER_TOKEN` — via a systemd
`EnvironmentFile=/etc/homeboy/broker.env` with `0600` permissions rather than an
inline `Environment=` line — so the secret never lands in `systemctl show`
output. Pair the credential on the broker host with `homeboy runner broker pair`
(see [Broker authentication and pairing](#broker-authentication-and-pairing)).

### `status`

```sh
homeboy runner status <runner-id>
```

Shows the persisted tunnel/session state for a runner. Use this to determine
whether `runner exec` will use a connected daemon or needs an explicit fallback.
The JSON payload uses `command: "runner.status"` and reports whether a saved
session exists, whether the tunnel still appears live, and the recorded endpoint
details.

Status output also carries the former Lab readiness handoff. `extra` includes
`preferred_lab_runner`, `selected_lab_runner.runner_homeboy`, and
`managed_followups` with copyable commands for recent run discovery, evidence
inspection, `runner doctor --scope lab-offload`, runner environment inspection,
daemon refresh, binary upgrade, managed `runner exec`, and workspace sync. Use
this output before assuming a runner has picked up a merged Homeboy CLI fix.

Runner-side extension updates use the generic runner/extension primitives instead
of a Lab-specific sync command:

```sh
homeboy runner exec <runner-id> -- homeboy extension install <source> --id <extension-id> --ref <ref> --replace
homeboy runner exec <runner-id> -- homeboy extension refresh <source> --id <extension-id> --ref <ref>
```

Status output is also the operator handoff for active runner jobs. When the
connected daemon or reverse broker can report active work, each active job
includes lifecycle timing fields such as `updated_at_ms`, `heartbeat_age_ms`,
`claim_id`, `claimed_by_runner_id`, `claimed_at_ms`, `claim_expires_at_ms`, and
`claim_expires_in_ms`. The top-level `operator_commands` array gives copyable
next actions for the current session, including job log following, cancellation,
broker claim reconciliation for reverse runners, and artifact lookup/fetch
commands when the status payload has enough context.

### `disconnect`

```sh
homeboy runner disconnect <runner-id>
```

Closes a persisted runner tunnel session and removes its local session state.
The JSON payload uses `command: "runner.disconnect"` and reports which session
state was removed. This is safe to run when no live session exists; it is the
explicit cleanup counterpart to `runner connect`.

### `list`

```sh
homeboy runner list
```

### `show`

```sh
homeboy runner show <id>
```

### `set`

```sh
homeboy runner set <id> --json <JSON>
homeboy runner set <id> --base64 <BASE64_JSON>
homeboy runner set <id> --json '{"workspace_root":"/srv/homeboy","concurrency_limit":4}'
```

Updates a runner by merging a JSON object into the runner config. SSH runner settings live under `servers/<id>.json` as the server's `runner` capability; local runners live under `runners/<id>.json`.
Arbitrary runner updates must use `--json` or `--base64`; positional `key=value` and trailing arbitrary `--key value` updates are not accepted.

### `trust`

```sh
homeboy runner trust <runner-id> --project <project-id> --command test --command bench --allow-raw-exec false
homeboy runner trust <runner-id> --workspace-root <runner-workspace-root> --artifact-policy metadata
homeboy runner trust <runner-id> --peer <controller-server-id> --fingerprint SHA256:...
```

Persists controller-side trust policy for a runner. Policy is stored in the runner config as `policy`, not in transient CLI state. Repeated values are appended without duplicates.

Policy fields:

- `--project <id>` allows a project to use the runner.
- `--command <family>` allows a command family such as `test`, `bench`, `lint`, `audit`, `trace`, `cargo`, or `runner.exec`.
- `--allow-raw-exec <true|false>` controls arbitrary `runner exec` shell access. SSH runner raw exec is denied by default until this is explicitly true.
- `--workspace-root <path>` limits execution to one or more approved runner workspace roots.
- `--artifact-policy <label>` records artifact behavior; `none` and `deny` block patch capture.
- `--peer <id>` records accepted peer/controller server IDs for reverse-runner pairing.
- `--fingerprint <value>` records expected peer host keys or equivalent fingerprints.

### `pair`

```sh
homeboy runner pair <runner-id> --peer <controller-server-id> --accept-project <project-id> --workspace-root <runner-workspace-root>
homeboy runner pair <runner-id> --fingerprint SHA256:... --allow-raw-exec false
```

Persists runner-side pairing policy for trusted controllers. `pair` writes the same durable `policy` object as `trust`, using runner-side option names for accepted peer IDs, accepted project IDs, peer fingerprints, workspace roots, and raw exec policy.

### `remove`

```sh
homeboy runner remove <id>
```

### `exec`

```sh
homeboy runner exec <runner-id> -- <command...>
homeboy runner exec <runner-id> --project <project-id> --cwd /runner/workspace/project -- <command...>
homeboy runner exec <runner-id> --ssh --cwd /runner/workspace/project -- <command...>
homeboy runner exec <runner-id> --run-id ssi-fixture-matrix-summary -- <command...>
homeboy runner exec <runner-id> --cwd /runner/workspace/project --require-path /runner/workspace/project -- <command...>
homeboy runner env <runner-id>
homeboy runner env <runner-id> --show-values
```

`exec` submits the command to the connected runner daemon when `homeboy runner connect <runner-id>` has established a live loopback tunnel. If no daemon session is connected, local runners execute directly and SSH runners require explicit diagnostic `--ssh`. SSH runner raw exec is policy-denied by default until `policy.allow_raw_exec` is explicitly true.

Path rules:

- SSH runners require `workspace_root` so local paths are not silently reused remotely.
- SSH `--cwd` must be an absolute path under the configured `workspace_root`.
- Omitting `--cwd` on an SSH runner uses the runner `workspace_root`.
- `--require-path <path>` preflights one or more runner-side paths before execution. Use it when a command references a lab worktree path so missing controller-only paths fail with a structured `require_path` error instead of an empty command failure.
- `--project <id>` feeds the runner trust policy project allowlist check.
- `--run-id <id>` sets the persisted controller-side runner-exec run id for ad hoc evidence commands. When omitted, Homeboy derives a run id prefix from the command domain, such as `trace-matrix-summary`.
- `--ssh` is the explicit diagnostic fallback when `connect` is unavailable; daemon execution is preferred because it records job metadata and supports artifact-oriented workflows.
- Diagnostic SSH output serializes as `mode: "diagnostic_ssh"` and does not include job/event evidence.
- Raw SSH execution remains intentionally explicit and should not be used as production Lab/offload evidence; use connected daemon or reverse broker execution for job/event/artifact-compatible output.
- Mutation-capable executions expose returned source changes through
  `mutation_artifacts.patch_ref` when a patch artifact is available. Direct
  daemon and reverse broker results use the same field; callers should not branch
  on transport mode to find returned mutations.

Runner job environment:

- `homeboy runner env <runner-id>` shows configured public runner env plus `secret_env` keys/references for runner jobs. It does not resolve or print secret values.
- Public `env` values are redacted by default because legacy configs may still contain tokens. Use `--show-values` only in trusted local/operator contexts; `secret_env` remains references-only even with `--show-values`.
- `homeboy ssh <server> -- printenv NAME` inspects the server login shell environment. It does not include runner job env unless the variable is also configured on the server shell.
- Use `homeboy runner exec <runner-id> -- printenv NAME` for final execution-time proof when debugging resolved runner job environment.

Runner metrics:

- Local runner execution, connected daemon jobs, and reverse-runner worker results include a `metrics` object with `duration_ms`, `sample_count`, and lightweight resource fields when available.
- On Linux runners, metrics are sampled from `/proc` for the command process tree and include `peak_rss_bytes`, `child_process_count_peak`, `cpu_user_ms`, and `cpu_system_ms`.
- CPU accounting is sampled and can miss very short-lived child processes between samples; duration is always recorded, and non-Linux runners report `source: "duration_only"`.

### `workspace list`

```sh
homeboy runner workspace list <runner-id>
homeboy runner workspace list <runner-id> --limit 5
```

`workspace list` shows recent runner-side Lab workspace directories under the runner's configured `workspace_root` and includes a reusable `runner exec --cwd <remote-path>` command for each entry. Use it after a manual or matrix session when the previous `remote_path` scrolled out of view and you need to run another command against the same runner-side checkout.

### `workspace sync`

```sh
homeboy runner workspace sync <runner-id> --path <local-worktree>
homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot
homeboy runner workspace sync <runner-id> --path <local-worktree> --mode git
```

`workspace sync` materializes a controller-side worktree under the runner's configured `workspace_root` so runner execution can run against an explicit remote path while Git operations and canonical edits stay local.

Modes:

- `snapshot` copies the current local tree, including dirty edits, through a tar stream from the controller. Use this for private or proxy-dependent sources because the runner does not need repository access.
- `git` requires a clean local tree, then clones or refreshes `remote.origin.url` on the runner and checks out local `HEAD` detached. Use this only when the runner is allowed to fetch the remote directly.

Private/proxied sources:

- Private or proxy-dependent source access stays on the controller machine.
- Materialize those sources with `homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot`.
- Use the returned `remote_path` for downstream `runner exec --cwd` or job inputs.
- Runner-side Git fetches for configured private/proxied hosts are refused with an actionable diagnostic. The default host list includes `github.example.com`; override with `HOMEBOY_PRIVATE_PROXIED_SOURCE_HOSTS` only when a runner is explicitly allowed to fetch those sources.

Safety rules:

- The remote path is deterministic and lives under `<workspace_root>/_lab_workspaces/`.
- Snapshot sync excludes dependency directories, build outputs, caches, `.git`, and common secret file patterns such as `.env*`, `*.pem`, and `*.key`.
- Runner policy can add project-specific generated-state patterns with `snapshot_excludes`; configured patterns are merged with the default snapshot safety excludes and affect snapshot hashing, stats, and materialization.
- Git sync refuses to overwrite an existing dirty runner-side checkout by default. Use `--allow-dirty-lab-workspace` only for noisy investigation where discarding runner-side changes is intentional; Lab metadata records that override.
- Output includes `local_path`, `remote_path`, `sync_mode`, `snapshot_identity`, and snapshot `files` / `bytes` when available.
- The runner workspace is execution-only; this command does not push branches, commit, or make the runner authoritative for source changes.

### `workspace apply`

```sh
homeboy runner workspace apply <runner-apply.json>
homeboy runner workspace apply <runner-apply.json> --force
```

`workspace apply` brings a runner-generated fix artifact back to the local source worktree recorded in the artifact's `source_snapshot.local_path`. It is local-only: it does not commit, push, or make the runner canonical. Reviewability stays in normal local Git via `git status` and `git diff`.

Safety rules:

- The artifact must identify the local source worktree through `source_snapshot.local_path`.
- Homeboy recalculates the current local `source_snapshot.snapshot_hash` before applying.
- If the local source worktree drifted since the Lab snapshot, apply is refused unless `--force` is explicit.
- Unified diffs are checked with `git apply --check` before mutation, so conflicts do not partially apply.
- Delta paths must be relative and stay inside the source worktree.
- Output includes `apply_status`, `modified_files`, `expected_snapshot_hash`, and `current_snapshot_hash`.

Temporary Wave 4 adapter contract, until the runner fix-capture contract settles:

```json
{
  "source_snapshot": {
    "runner_id": "lab-a",
    "local_path": "/path/to/project@branch",
    "remote_path": "/srv/homeboy/_lab_workspaces/project-abc123",
    "git_sha": "...",
    "dirty": false,
    "sync_mode": "snapshot",
    "snapshot_hash": "sha256:...",
    "synced_at": "2026-05-16T00:00:00Z",
    "sync_excludes": [".git/", "node_modules/"]
  },
  "patch": {
    "format": "unified_diff",
    "content": "diff --git a/file.txt b/file.txt\n..."
  }
}
```

Delta form is also accepted for explicit file replacement/deletion:

```json
{
  "source_snapshot": { "...": "..." },
  "delta": {
    "files": [
      { "path": "src/file.txt", "content_base64": "Li4u" },
      { "path": "obsolete.txt", "delete": true }
    ]
  }
}
```

## Runner Shape

SSH runner records are stored on their server as `runner` capability config under `~/.config/homeboy/servers/<id>.json`.

```json
{
  "id": "runner-a",
  "host": "runner.example.internal",
  "user": "runner",
  "port": 22,
  "runner": {
    "workspace_root": "/srv/homeboy/workspaces",
    "homeboy_path": "/usr/local/bin/homeboy",
    "daemon": false,
    "concurrency_limit": 4,
    "artifact_policy": "copy",
    "env": {},
    "resources": {}
  }
}
```

Standalone local runner records are still stored under `~/.config/homeboy/runners/`.

```json
{
  "id": "lab-local",
  "kind": "local",
  "server_id": null,
  "workspace_root": "/srv/homeboy/workspaces",
  "homeboy_path": "/usr/local/bin/homeboy",
  "daemon": false,
  "concurrency_limit": 2,
  "artifact_policy": "copy",
  "env": {},
  "resources": {}
}
```

Rules:

- `kind` is `local` or `ssh`.
- `ssh` runner IDs are server IDs; a single SSH machine does not need a separate runner ID.
- `concurrency_limit`, when set, must be greater than zero.
- `env` and `resources` are metadata maps for future `connect`, `doctor`, `exec`, and Desktop workflows.

## JSON Output

All command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The `data` payload uses the generic entity CRUD shape:

- `command`: action identifier such as `runner.add`, `runner.enable`, `runner.list`, `runner.show`, `runner.set`, `runner.remove`, `runner.doctor`, `runner.connect`, `runner.status`, `runner.disconnect`, `runner.exec`, `runner.workspace.sync`, or `runner.workspace.apply`
- `id`: present for single-runner actions
- `entity`: runner configuration for single-runner read/write actions
- `entities`: list for `list`
- `updated_fields`: list of updated field names for writes
- `deleted`: list of removed runner IDs
