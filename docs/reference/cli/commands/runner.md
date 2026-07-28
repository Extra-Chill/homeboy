<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy runner` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/runner.md](../../../commands/runner.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy runner`

```sh
homeboy runner <COMMAND>
```

Manage local and SSH execution runners

| Subcommand | Summary |
| --- | --- |
| `homeboy runner add` | Register a local or SSH execution runner |
| `homeboy runner enable` | Enable runner capability on an existing SSH server |
| `homeboy runner list` | List all configured runners |
| `homeboy runner show` | Display runner configuration |
| `homeboy runner set` | Modify runner settings |
| `homeboy runner trust` | Trust a runner for constrained controller-side project execution |
| `homeboy runner pair` | Pair a runner with a trusted peer/controller policy from the runner side |
| `homeboy runner remove` | Remove a runner configuration |
| `homeboy runner doctor` | Diagnose a local or configured SSH runner without mutating it |
| `homeboy runner connect` | Connect to a runner by starting a loopback-only remote daemon and SSH tunnel |
| `homeboy runner status` | Show persisted runner tunnel status |
| `homeboy runner disconnect` | Close a runner tunnel and remove its persisted session state |
| `homeboy runner refresh-homeboy` | Build or select the Homeboy binary used for runner/Lab jobs |
| `homeboy runner dev-sync` | Sync a controller-local Homeboy dev binary to the runner and select it for Lab jobs |
| `homeboy runner cache-prune` | Inventory or remove stale managed Homeboy binary slots on a runner |
| `homeboy runner exec` | Execute a command on a configured runner. Use `homeboy runner exec [HOMEBOY_OPTIONS] <RUNNER> -- <COMMAND>...` |
| `homeboy runner env` | Show the effective environment injected into runner jobs |
| `homeboy runner lifecycle` | Evaluate runner workspace lifecycle and finalization readiness without mutating state |
| `homeboy runner job` | Inspect or follow a runner daemon job stream |
| `homeboy runner work` | Claim and execute one brokered reverse-runner job from this machine |
| `homeboy runner workspace` | Materialize local workspaces on a configured runner |
| `homeboy runner refresh-plan` | Plan a runner-backed refresh loop before dispatching matrix-style work |
| `homeboy runner broker` | Manage reverse runner broker authentication and pairing |

## `homeboy runner add`

```sh
homeboy runner add [OPTIONS] [ID]
```

Register a local or SSH execution runner

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON input spec for add/update (supports single or bulk) |
| `--skip-existing` | flag | Skip items that already exist (JSON mode only) |
| `--kind` | `<KIND>` | Runner kind. Defaults to ssh when --server is set, otherwise local Values: `local`, `ssh`. |
| `--server` | `<SERVER>` | Existing server ID for SSH runners |
| `--workspace-root` | `<WORKSPACE_ROOT>` | Root directory where this runner checks out or owns workspaces |
| `--homeboy-path` | `<HOMEBOY_PATH>` | Homeboy binary path on the runner machine |
| `--daemon` | flag | Prefer daemon-backed execution for future runner commands |
| `--concurrency-limit` | `<CONCURRENCY_LIMIT>` | Maximum concurrent workflows this runner should accept |
| `--artifact-policy` | `<ARTIFACT_POLICY>` | Artifact retention/copying policy label for future execution commands |

## `homeboy runner enable`

```sh
homeboy runner enable [OPTIONS] <SERVER_ID>
```

Enable runner capability on an existing SSH server

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID to make runner-capable |

| Option | Value | Description |
| --- | --- | --- |
| `--workspace-root` | `<WORKSPACE_ROOT>` | Root directory where this server checks out or owns workspaces |
| `--homeboy-path` | `<HOMEBOY_PATH>` | Homeboy binary path on the server machine |
| `--daemon` | flag | Prefer daemon-backed execution for future runner commands |
| `--concurrency-limit` | `<CONCURRENCY_LIMIT>` | Maximum concurrent workflows this server should accept |
| `--artifact-policy` | `<ARTIFACT_POLICY>` | Artifact retention/copying policy label for future execution commands |

## `homeboy runner list`

```sh
homeboy runner list
```

List all configured runners

## `homeboy runner show`

```sh
homeboy runner show <ID>
```

Display runner configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Runner ID |

## `homeboy runner set`

```sh
homeboy runner set [OPTIONS] [ID]
```

Modify runner settings

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Entity ID (optional if provided in JSON body) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON object to merge into the entity (supports @file and - for stdin) |
| `--base64` | `<BASE64>` | Base64-encoded JSON object (bypasses shell escaping issues) |
| `--replace` | `<FIELD>` | Replace these fields instead of merging arrays |

## `homeboy runner trust`

```sh
homeboy runner trust [OPTIONS] <RUNNER_ID>
```

Trust a runner for constrained controller-side project execution

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECTS>` | Project ID allowed to use this runner. Repeat for multiple projects |
| `--command` | `<COMMANDS>` | Allowed command family, for example test, bench, lint, audit, trace, cargo, or runner.exec. Repeat or pass comma-separated values |
| `--allow-raw-exec` | `<ALLOW_RAW_EXEC>` | Explicitly allow or deny raw runner exec shell commands Values: `true`, `false`. |
| `--allow-homeboy-convergence` | `<ALLOW_HOMEBOY_CONVERGENCE>` | Explicitly allow controller-driven Homeboy binary convergence Values: `true`, `false`. |
| `--workspace-root` | `<WORKSPACE_ROOTS>` | Workspace root allowed by policy. Repeat for multiple roots |
| `--artifact-policy` | `<ARTIFACT_POLICY>` | Artifact behavior for runner jobs, for example copy, metadata, none, or deny |
| `--peer` | `<PEERS>` | Expected peer/controller server ID. Repeat for multiple peers |
| `--fingerprint` | `<FINGERPRINTS>` | Expected peer host key/fingerprint. Repeat for multiple fingerprints |

## `homeboy runner pair`

```sh
homeboy runner pair [OPTIONS] <RUNNER_ID>
```

Pair a runner with a trusted peer/controller policy from the runner side

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--peer` | `<PEERS>` | Peer/controller server ID accepted by this runner. Repeat for multiple peers |
| `--fingerprint` | `<FINGERPRINTS>` | Peer/controller host key/fingerprint. Repeat for multiple fingerprints |
| `--accept-project` | `<PROJECTS>` | Project ID accepted from the peer. Repeat for multiple projects |
| `--workspace-root` | `<WORKSPACE_ROOTS>` | Workspace root this runner accepts jobs under. Repeat for multiple roots |
| `--allow-raw-exec` | `<ALLOW_RAW_EXEC>` | Explicitly allow or deny raw runner exec shell commands Values: `true`, `false`. |
| `--allow-homeboy-convergence` | `<ALLOW_HOMEBOY_CONVERGENCE>` | Explicitly allow controller-driven Homeboy binary convergence Values: `true`, `false`. |

## `homeboy runner remove`

```sh
homeboy runner remove <ID>
```

Remove a runner configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Runner ID |

## `homeboy runner doctor`

```sh
homeboy runner doctor [OPTIONS] <RUNNER_ID>
```

Diagnose a local or configured SSH runner without mutating it

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID. Use `local`, `localhost`, or `self` for this machine; other values resolve through `homeboy runner` configuration |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Component/workspace path to use as the extension parity probe cwd |
| `--extension` | `<REQUIRED_EXTENSIONS>` | Required extension ID to resolve on the runner. Repeat for multiple extensions |
| `--require-tool` | `<REQUIRED_TOOLS>` | Required command to resolve on the runner PATH. Repeat for provider/job-specific tools |
| `--scope` | `<SCOPE>` | Readiness scope. `lab-offload` adds Lab-specific binary, daemon, and provider readiness checks Values: `general`, `lab-offload`, `secret-env`. |
| `--repair` | flag | Safely repair issues in the selected scope, such as reconnecting a stale Lab daemon |

## `homeboy runner connect`

```sh
homeboy runner connect [OPTIONS] <ID>
```

Connect to a runner by starting a loopback-only remote daemon and SSH tunnel

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Runner ID for direct SSH connect, or controller/broker ID when --reverse is set |

| Option | Value | Description |
| --- | --- | --- |
| `--reverse` | flag | Record a runner-initiated reverse tunnel session substrate |
| `--reverse-runner` | `<REVERSE_RUNNER>` | Runner ID initiating the reverse connection |
| `--broker-url` | `<BROKER_URL>` | Broker/controller URL observed by the reverse runner |
| `--adopt-orphan-lease` | `<ADOPT_ORPHAN_LEASE>` | Explicitly adopt this exact remote daemon lease after confirming its PID is dead |
| `--confirm-pid-dead` | flag | Deprecated no-op retained for one release; the runner proves the recorded PID dead itself |
| `--adopt-live-lease` | `<ADOPT_LIVE_LEASE>` | Operator-confirm a live lease/PID/build adoption within the trusted remote SSH UID boundary; never stops or replaces a daemon |
| `--expected-live-pid` | `<EXPECTED_LIVE_PID>` | Current remote daemon PID paired with --adopt-live-lease |
| `--confirm-untracked-child-dead` | `<CONFIRM_UNTRACKED_CHILD_DEAD>` | Confirm one exact unresolved job has no live untracked child; repeat for each job |
| `--reconcile-leaseless-orphans` | flag | Explicitly reconcile active jobs after proving the missing-lease remote store has no daemon owner |
| `--confirm-no-daemon-owner` | flag | Deprecated no-op retained for one release; the runner fails closed on owner-lock, process, and listener probes |
| `--recover-missing-lease-state` | `<RECOVER_MISSING_LEASE_STATE>` | Recover this exact lease after the remote daemon state record was lost |
| `--recorded-pid` | `<RECORDED_PID>` | Recorded remote daemon PID paired with --recover-missing-lease-state |
| `--recorded-endpoint` | `<RECORDED_ENDPOINT>` | Recorded concrete remote daemon endpoint paired with --recover-missing-lease-state |
| `--confirm-control-plane-lost` | flag | Deprecated no-op retained for one release; the runner probes its own state record and endpoint |

## `homeboy runner status`

```sh
homeboy runner status [OPTIONS] [ID]
```

Show persisted runner tunnel status

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Runner ID. Omit to show all runner session states |

| Option | Value | Description |
| --- | --- | --- |
| `--generations` | flag | Include the full historical draining-generation inventory. By default status leads with the compact authoritative admission summary and omits the expanded per-generation ledger, which can run to thousands of lines on a long-lived runner |
| `--full` | flag | Return complete status, runtime diagnostics, followups, and generation detail |

## `homeboy runner disconnect`

```sh
homeboy runner disconnect [OPTIONS] <ID>
```

Close a runner tunnel and remove its persisted session state

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--local-recovery` | flag | Remove only this controller's matching local tunnel/session state without contacting the remote runner |

## `homeboy runner refresh-homeboy`

```sh
homeboy runner refresh-homeboy [OPTIONS] <RUNNER_ID>
```

Build or select the Homeboy binary used for runner/Lab jobs

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--select` | `<SELECT>` | Existing runner-side Homeboy binary to select instead of building one |
| `--source` | `<SOURCE>` | Git remote URL to clone/fetch when materializing a managed Homeboy binary |
| `--ref` | `<GIT_REF>` | Git ref to materialize from the source remote |
| `--target-dir` | `<TARGET_DIR>` | Runner-side checkout directory for the managed Homeboy source |
| `--reconnect` | flag | Disconnect and reconnect the runner daemon after updating homeboy_path |
| `--force` | flag | Interrupt active daemon jobs when reconnecting |
| `--allow-downgrade` | flag | Permit replacing a newer managed runner build with an older Git revision |
| `--dry-run` | flag | Print the plan without executing it or changing runner config |

## `homeboy runner dev-sync`

```sh
homeboy runner dev-sync [OPTIONS] <RUNNER_ID>
```

Sync a controller-local Homeboy dev binary to the runner and select it for Lab jobs

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--homeboy-source` | `<HOMEBOY_SOURCE>` | Controller-local Homeboy source checkout to build before upload. Defaults to current directory |
| `--homeboy-binary` | `<HOMEBOY_BINARY>` | Controller-local prebuilt Homeboy binary to upload instead of building from source |
| `--extensions` | `<EXTENSIONS>` | Dev extension source to sync later, in id=path form. Accepted and recorded; extension relink is deferred |
| `--reconnect` | flag | Disconnect and reconnect the runner daemon after selecting the dev binary |
| `--dry-run` | flag | Print the plan without executing it or changing runner config |

## `homeboy runner cache-prune`

```sh
homeboy runner cache-prune [OPTIONS] <RUNNER_ID>
```

Inventory or remove stale managed Homeboy binary slots on a runner

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Delete eligible slots. Omit for inventory only |
| `--min-age-hours` | `<MIN_AGE_HOURS>` | Minimum slot age before an unselected slot is eligible. Defaults to the shared runner age floor (`cleanup::RUNNER_MIN_AGE_HOURS`) |

## `homeboy runner exec`

```sh
homeboy runner exec [OPTIONS] <ID> [COMMAND]...
```

Execute a command on a configured runner. Use `homeboy runner exec [HOMEBOY_OPTIONS] <RUNNER> -- <COMMAND>...`

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Runner ID |
| `[COMMAND]...` | no | Command and arguments to execute on the runner |

| Option | Value | Description |
| --- | --- | --- |
| `--cwd` | `<CWD>` | Remote/current working directory. SSH runners require this to be inside the runner workspace root unless the runner has a default workspace_root |
| `--sync-workspace` | `<SYNC_WORKSPACE>` | Snapshot a local worktree to the runner first and execute from the materialized remote path |
| `--project` | `<PROJECT>` | Project ID used for runner trust policy checks |
| `--ssh` | flag | Allow diagnostic-only SSH command execution when no daemon session is connected |
| `--capture-patch` | flag | Capture the file delta produced by the remote command as a patch artifact |
| `--require-path` | `<REQUIRE_PATHS>` | Runner-side path that must exist before executing the command. Repeat for multiple paths |
| `--script-file` | `<SCRIPT_FILE>` | Read a shell script from this path and execute it on the runner with bash. Use `-` to read the script from stdin |
| `--env` | `<ENV>` | Environment variable to inject into the runner process as KEY=VALUE. Repeat for multiple values |
| `--secret-env` | `<NAME>` | Secret environment variable name to resolve through the runner secret-env contract. Repeat for multiple names |
| `--secret-env-plan` | `<JSON>` | Secret-env plan JSON to apply to the runner process |
| `--secret-env-plan-file` | `<PATH>` | Path to a secret-env plan JSON file to apply to the runner process |
| `--extension-env` | `<ID>` | Installed extension that contributes runtime environment on the selected runner. Repeat in contribution order |
| `--dry-run` | flag | Build the runner exec plan without executing it |
| `--run-id` | `<RUN_ID>` | Explicit persisted run id for ad hoc runner exec evidence |
| `--artifact` | `<PATH>` | File or directory path produced by the runner command to persist as a run artifact. Relative paths are resolved from the runner exec cwd. Repeat for multiple artifacts |
| `--artifact-dir` | `<PATH>` | Directory whose immediate produced files/directories should each be persisted as run artifacts. Relative paths are resolved from the runner exec cwd. Repeat for multiple directories |
| `--summary` | `<PATH>` | Summary file or directory produced by the runner command to persist as typed run evidence. Relative paths are resolved from the runner exec cwd. Repeat for multiple summaries |
| `--json` | flag | Print the full structured runner execution envelope to stdout |
| `--raw` | flag | Print remote stdout/stderr directly instead of the structured JSON envelope. Use global --output to still write the full structured envelope to a file |
| `--read-only-artifact` | flag | Treat this exec as a read-only retrieval of evidence the runner already retains (for example, hydrating a completed run's artifact). Routes to the generation that owns the retained run/artifact and never rotates the shared tunnel, so a stale admission daemon does not block the read |

## `homeboy runner env`

```sh
homeboy runner env <ID>
```

Show the effective environment injected into runner jobs

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Runner ID |

## `homeboy runner lifecycle`

```sh
homeboy runner lifecycle [OPTIONS] <RUNNER_ID>
```

Evaluate runner workspace lifecycle and finalization readiness without mutating state

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID that owns the workspace |

| Option | Value | Description |
| --- | --- | --- |
| `--workspace` | `<WORKSPACE>` | Absolute runner-side workspace path |
| `--job-id` | `<JOB_ID>` | Runner daemon or broker job ID associated with this workspace |
| `--run-id` | `<RUN_ID>` | Durable run ID associated with this workspace |
| `--status` | `<STATUS>` | Canonical lifecycle status. When omitted, --exit-code maps 0 to succeeded and non-zero to failed Values: `unknown`, `queued`, `running`, `succeeded`, `partial-failure`, `failed`, `cancelled`, `timed-out`, `stale`. |
| `--exit-code` | `<EXIT_CODE>` | Process exit code to project into lifecycle status and RunOutcomeEnvelope fields |

## `homeboy runner job`

```sh
homeboy runner job <COMMAND>
```

Inspect or follow a runner daemon job stream

| Subcommand | Summary |
| --- | --- |
| `homeboy runner job logs` | Show or follow durable runner daemon job events |
| `homeboy runner job cancel` | Cancel a queued or running durable runner daemon job |
| `homeboy runner job reconcile` | Reconcile expired reverse-runner broker claims |
| `homeboy runner job artifacts` | Inspect broker-held reverse-runner artifact metadata |

## `homeboy runner job logs`

```sh
homeboy runner job logs [OPTIONS] <RUNNER_ID> <JOB_ID>
```

Show or follow durable runner daemon job events

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID with an active daemon connection |
| `<JOB_ID>` | yes | Runner daemon job ID from runner exec/Lab output or error details |

| Option | Value | Description |
| --- | --- | --- |
| `--follow` | flag | Poll until the remote job reaches a terminal state, printing new events to stderr |
| `--poll-ms` | `<POLL_MS>` | Poll interval in milliseconds when --follow is set |
| `--cursor` | `<CURSOR>` | Resume after this previously displayed event sequence |
| `--compact` | flag | Return only lifecycle events, exit code, and a bounded stdout/stderr tail |
| `--tail` | `<KB>` | Bound embedded stdout/stderr to the last N kilobytes, surfaced as a tail |

## `homeboy runner job cancel`

```sh
homeboy runner job cancel <RUNNER_ID> <JOB_ID>
```

Cancel a queued or running durable runner daemon job

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID with an active daemon connection |
| `<JOB_ID>` | yes | Runner daemon job ID from runner exec/Lab output or error details |

## `homeboy runner job reconcile`

```sh
homeboy runner job reconcile <RUNNER_ID>
```

Reconcile expired reverse-runner broker claims

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Reverse-connected runner ID |

## `homeboy runner job artifacts`

```sh
homeboy runner job artifacts <RUNNER_ID> <JOB_ID> <ARTIFACT_ID>
```

Inspect broker-held reverse-runner artifact metadata

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Reverse-connected runner ID |
| `<JOB_ID>` | yes | Reverse broker job ID |
| `<ARTIFACT_ID>` | yes | Artifact ID reported by the finished broker job |

## `homeboy runner work`

```sh
homeboy runner work [OPTIONS] <RUNNER_ID>
```

Claim and execute one brokered reverse-runner job from this machine

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID on this machine |

| Option | Value | Description |
| --- | --- | --- |
| `--broker-url` | `<BROKER_URL>` | Controller/broker daemon URL |
| `--broker-token` | `<BROKER_TOKEN>` | Paired broker bearer token. Falls back to the HOMEBOY_BROKER_TOKEN environment variable when omitted. Required when the broker enforces auth; omit only for loopback-open smoke setups |
| `--project` | `<PROJECT>` | Optional project filter for claimed jobs |
| `--lease-ms` | `<LEASE_MS>` | Claim lease duration in milliseconds |
| `--loop` | flag | Keep claiming jobs until SIGINT/SIGTERM instead of exiting after one claim |
| `--idle-backoff-ms` | `<IDLE_BACKOFF_MS>` | Initial sleep after an empty claim in loop mode |
| `--max-idle-backoff-ms` | `<MAX_IDLE_BACKOFF_MS>` | Maximum sleep after repeated empty claims in loop mode |
| `--broker-failure-backoff-ms` | `<BROKER_FAILURE_BACKOFF_MS>` | Sleep after transient broker failures in loop mode |
| `--broker-retry-limit` | `<BROKER_RETRY_LIMIT>` | Consecutive broker failures allowed before the worker exits non-zero |

## `homeboy runner workspace`

```sh
homeboy runner workspace <COMMAND>
```

Materialize local workspaces on a configured runner

| Subcommand | Summary |
| --- | --- |
| `homeboy runner workspace list` | List recent runner-side Lab workspaces and reusable exec commands |
| `homeboy runner workspace snapshots` | Discover metadata-backed runner workspace snapshots by repo, ref, commit, or run |
| `homeboy runner workspace sync` | Materialize a controller-side worktree into the runner workspace root |
| `homeboy runner workspace update` | Apply a source delta to a prepared workspace selected by its snapshot lease |
| `homeboy runner workspace pull` | Copy selected files from a runner workspace back to the controller |
| `homeboy runner workspace apply` | Apply a Lab-generated patch/delta back to its local source worktree |
| `homeboy runner workspace prune` | Preview or remove orphaned runner-side Lab workspaces |

## `homeboy runner workspace list`

```sh
homeboy runner workspace list [OPTIONS] <RUNNER_ID>
```

List recent runner-side Lab workspaces and reusable exec commands

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--limit` | `<LIMIT>` | Maximum number of workspaces to return |

## `homeboy runner workspace snapshots`

```sh
homeboy runner workspace snapshots [OPTIONS] <RUNNER_ID>
```

Discover metadata-backed runner workspace snapshots by repo, ref, commit, or run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--repo` | `<REPO>` | Source repository name, normally the local workspace basename before any @slug suffix |
| `--source-ref` | `<SOURCE_REF>` | Source git ref captured when the snapshot was synced |
| `--source-commit` | `<SOURCE_COMMIT>` | Source git commit captured when the snapshot was synced |
| `--run` | `<RUN_ID>` | Agent-task or Lab run id captured in snapshot metadata when available |
| `--limit` | `<LIMIT>` | Maximum number of snapshots to return |

## `homeboy runner workspace sync`

```sh
homeboy runner workspace sync [OPTIONS] <RUNNER_ID>
```

Materialize a controller-side worktree into the runner workspace root

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Local worktree path to materialize for Lab execution |
| `--mode` | `<MODE>` | Sync mode. snapshot streams source from the controller; snapshot-git also initializes a synthetic git checkout; git is only for clean public/runner-accessible remotes Values: `snapshot`, `snapshot-git`, `git`. |
| `--allow-dirty-lab-workspace` | flag | Permit git sync to overwrite a dirty runner-side workspace |

## `homeboy runner workspace update`

```sh
homeboy runner workspace update [OPTIONS] <RUNNER_ID>
```

Apply a source delta to a prepared workspace selected by its snapshot lease

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Local worktree containing the updated source |
| `--lease` | `<LEASE>` | Opaque prepared-workspace lease returned by workspace sync or a previous update |

## `homeboy runner workspace pull`

```sh
homeboy runner workspace pull [OPTIONS] <RUNNER_ID>
```

Copy selected files from a runner workspace back to the controller

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--remote-path` | `<REMOTE_PATH>` | Absolute runner-side workspace or snapshot path to pull from |
| `--include` | `<INCLUDES>` | Relative glob to copy from the remote path. Repeat for multiple globs |
| `--to` | `<TO>` | Local destination directory on the controller |
| `--dry-run` | flag | Validate and print the copy plan without transferring files |

## `homeboy runner workspace apply`

```sh
homeboy runner workspace apply [OPTIONS] <INPUT>
```

Apply a Lab-generated patch/delta back to its local source worktree

| Argument | Required | Description |
| --- | --- | --- |
| `<INPUT>` | yes | Lab apply JSON artifact path |

| Option | Value | Description |
| --- | --- | --- |
| `--force` | flag | Apply even when the local worktree snapshot no longer matches the Lab source snapshot |

## `homeboy runner workspace prune`

```sh
homeboy runner workspace prune [OPTIONS] <RUNNER_ID>
```

Preview or remove orphaned runner-side Lab workspaces

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNNER_ID>` | yes | Runner ID |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Delete the previewed orphaned workspaces. Without this flag, the command is a dry run |
| `--min-age-hours` | `<MIN_AGE_HOURS>` | Minimum workspace age before it can be considered orphaned. Defaults to the shared runner age floor (`cleanup::RUNNER_MIN_AGE_HOURS`) |
| `--limit` | `<LIMIT>` | Maximum number of orphan candidates to report or remove per pass. Defaults to the shared page size (`cleanup::RUNNER_WORKSPACE_PAGE_LIMIT`) |
| `--passes` | `<PASSES>` | Maximum apply passes to run. Each pass re-scans and removes at most --limit candidates |
| `--cursor` | `<CURSOR>` | Opaque continuation cursor returned by an incomplete workspace-prune scan |

## `homeboy runner refresh-plan`

```sh
homeboy runner refresh-plan [OPTIONS] [COMMAND]...
```

Plan a runner-backed refresh loop before dispatching matrix-style work

| Argument | Required | Description |
| --- | --- | --- |
| `[COMMAND]...` | no | Command and arguments to run after the plan checks pass |

| Option | Value | Description |
| --- | --- | --- |
| `--runner` | `<RUNNER>` | Runner ID that will execute the workload |
| `--workspace` | `<WORKSPACE>` | Controller-side workspace or worktree to sync to the runner |
| `--runner-cwd` | `<RUNNER_CWD>` | Runner-side cwd for the eventual runner exec command |
| `--run-id` | `<RUN_ID>` | Stable run id to use for the produced evidence |
| `--produces` | `<PATH>` | Produced output directory or file. Relative paths are resolved from --runner-cwd |
| `--summary` | `<PATH>` | Produced summary directory or file. Relative paths are resolved from --runner-cwd |
| `--source` | `<PATH>` | Source path that must exist before the refresh is dispatched. Repeat for multiple paths |
| `--fixture` | `<PATH>` | Fixture path that must exist before the refresh is dispatched. Repeat for multiple paths |
| `--sync-mode` | `<SYNC_MODE>` | Runner workspace sync mode to use in the planned sync command |

## `homeboy runner broker`

```sh
homeboy runner broker <COMMAND>
```

Manage reverse runner broker authentication and pairing

| Subcommand | Summary |
| --- | --- |
| `homeboy runner broker pair` | Pair a runner with the broker, minting a one-time scoped bearer token |
| `homeboy runner broker revoke` | Revoke a paired credential by id |
| `homeboy runner broker list` | List paired broker credentials (never prints tokens) |

## `homeboy runner broker pair`

```sh
homeboy runner broker pair [OPTIONS] <ID>
```

Pair a runner with the broker, minting a one-time scoped bearer token

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Stable credential id used for later revocation |

| Option | Value | Description |
| --- | --- | --- |
| `--runner-id` | `<RUNNER_ID>` | Runner id this credential authorizes (worker routes must match it) |
| `--submit` | flag | Grant the controller submit scope (POST /runner/jobs) |
| `--work` | flag | Grant the worker scope (register/claim/event/finish/heartbeat) |
| `--no-install` | flag | Store only on this controller; skip installing broker_auth.json on an SSH runner host |

## `homeboy runner broker revoke`

```sh
homeboy runner broker revoke <ID>
```

Revoke a paired credential by id

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Credential id to revoke |

## `homeboy runner broker list`

```sh
homeboy runner broker list
```

List paired broker credentials (never prints tokens)
