<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy daemon` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/daemon.md](../../../commands/daemon.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy daemon`

```sh
homeboy daemon <COMMAND>
```

Run the local-only HTTP API daemon

| Subcommand | Summary |
| --- | --- |
| `homeboy daemon start` | Start the local daemon in the background |
| `homeboy daemon ensure-running` | Return the current live daemon or start one when no live daemon exists |
| `homeboy daemon adopt-orphan` | Explicitly replace one proven-dead daemon lease and reconcile its durable jobs |
| `homeboy daemon reconcile-dead-lease-orphans` | Reconcile an exact PID-less job set after one proven unexpected daemon exit |
| `homeboy daemon recover-missing-child-identity` | Recover one legacy job with exact PID and Linux start-tick evidence |
| `homeboy daemon reconcile-leaseless-orphans` | Explicitly reconcile active jobs after proving a missing-lease store has no daemon owner |
| `homeboy daemon recover-missing-lease-state` | Recover one exact lease after its daemon state record was lost |
| `homeboy daemon serve` | Run the local daemon in the foreground |
| `homeboy daemon stop` | Stop the background daemon recorded in the state file |
| `homeboy daemon status` | Show daemon state and selected local address |
| `homeboy daemon broker-config` | Render deployable reverse-runner broker service configuration |
| `homeboy daemon artifact-get` | Fetch artifact bytes through the local daemon byte endpoint |

## `homeboy daemon start`

```sh
homeboy daemon start [OPTIONS]
```

Start the local daemon in the background

| Option | Value | Description |
| --- | --- | --- |
| `--addr` | `<ADDR>` | Local bind address. Defaults to an OS-selected loopback port |

## `homeboy daemon ensure-running`

```sh
homeboy daemon ensure-running [OPTIONS]
```

Return the current live daemon or start one when no live daemon exists

| Option | Value | Description |
| --- | --- | --- |
| `--addr` | `<ADDR>` | _no help text_ |

## `homeboy daemon adopt-orphan`

```sh
homeboy daemon adopt-orphan [OPTIONS]
```

Explicitly replace one proven-dead daemon lease and reconcile its durable jobs

| Option | Value | Description |
| --- | --- | --- |
| `--lease-id` | `<LEASE_ID>` | Exact lease ID reported by `homeboy daemon status` |
| `--confirm-pid-dead` | flag | Deprecated no-op retained for one release; adoption already proves the recorded PID dead under the daemon lifecycle lock |
| `--recover-missing-child-identity` | flag | Accepted migration alias for legacy child recovery. It never mutates jobs |
| `--confirm-untracked-child-dead` | `<CONFIRM_UNTRACKED_CHILD_DEAD>` | Confirm the one expired PID-less reservation to terminalize before replacement |
| `--addr` | `<ADDR>` | _no help text_ |

## `homeboy daemon reconcile-dead-lease-orphans`

```sh
homeboy daemon reconcile-dead-lease-orphans [OPTIONS]
```

Reconcile an exact PID-less job set after one proven unexpected daemon exit

| Option | Value | Description |
| --- | --- | --- |
| `--lease-id` | `<LEASE_ID>` | _no help text_ |
| `--job-id` | `<JOB_IDS>` | Exact, complete active durable-job set to terminalize. The store recomputes the active set and refuses any mismatch, so this is a compare-and-swap over the destructive scope, not a fact assertion |
| `--confirm-pid-dead` | flag | Deprecated no-op retained for one release; recovery already requires persisted unexpected-termination evidence and re-proves the PID dead |
| `--confirm-workload-processes-absent` | flag | Required. Attests that the workload processes for --job-id were inspected and are absent. Unverifiable by design: this command exists because the daemon died before persisting any child identity, so the store holds no PID to check. Persisted as durable job provenance |
| `--addr` | `<ADDR>` | _no help text_ |

## `homeboy daemon recover-missing-child-identity`

```sh
homeboy daemon recover-missing-child-identity [OPTIONS]
```

Recover one legacy job with exact PID and Linux start-tick evidence

| Option | Value | Description |
| --- | --- | --- |
| `--lease-id` | `<LEASE_ID>` | _no help text_ |
| `--recorded-daemon-pid` | `<RECORDED_DAEMON_PID>` | _no help text_ |
| `--recorded-daemon-endpoint` | `<RECORDED_DAEMON_ENDPOINT>` | _no help text_ |
| `--job-id` | `<JOB_ID>` | _no help text_ |
| `--child-pid` | `<CHILD_PID>` | _no help text_ |
| `--child-starttime-ticks` | `<CHILD_STARTTIME_TICKS>` | _no help text_ |

## `homeboy daemon reconcile-leaseless-orphans`

```sh
homeboy daemon reconcile-leaseless-orphans [OPTIONS]
```

Explicitly reconcile active jobs after proving a missing-lease store has no daemon owner

| Option | Value | Description |
| --- | --- | --- |
| `--confirm-no-daemon-owner` | flag | _no help text_ |
| `--addr` | `<ADDR>` | _no help text_ |

## `homeboy daemon recover-missing-lease-state`

```sh
homeboy daemon recover-missing-lease-state [OPTIONS]
```

Recover one exact lease after its daemon state record was lost

| Option | Value | Description |
| --- | --- | --- |
| `--lease-id` | `<LEASE_ID>` | Exact lease ID captured before the daemon state record was lost |
| `--recorded-pid` | `<RECORDED_PID>` | Recorded daemon PID captured with the lease ID |
| `--recorded-endpoint` | `<RECORDED_ENDPOINT>` | Recorded concrete loopback endpoint captured with the lease ID |
| `--confirm-pid-dead` | flag | Deprecated no-op retained for one release; recovery already refuses a running recorded PID |
| `--confirm-control-plane-lost` | flag | Deprecated no-op retained for one release; recovery already requires an absent state record, a `lease_missing` freshness code, an unreachable daemon, and a failed probe of the recorded endpoint |
| `--addr` | `<ADDR>` | _no help text_ |

## `homeboy daemon serve`

```sh
homeboy daemon serve [OPTIONS]
```

Run the local daemon in the foreground

| Option | Value | Description |
| --- | --- | --- |
| `--addr` | `<ADDR>` | Local bind address. Defaults to an OS-selected loopback port |

## `homeboy daemon stop`

```sh
homeboy daemon stop [OPTIONS]
```

Stop the background daemon recorded in the state file

| Option | Value | Description |
| --- | --- | --- |
| `--lease-id` | `<LEASE_ID>` | Require this exact live daemon lease before stopping |
| `--force` | flag | Directly SIGTERM a matching stale or unreachable daemon lease. Requires --lease-id |

## `homeboy daemon status`

```sh
homeboy daemon status
```

Show daemon state and selected local address

## `homeboy daemon broker-config`

```sh
homeboy daemon broker-config [OPTIONS]
```

Render deployable reverse-runner broker service configuration

| Option | Value | Description |
| --- | --- | --- |
| `--listen-addr` | `<LISTEN_ADDR>` | Stable loopback address for the VPS service |
| `--binary-path` | `<BINARY_PATH>` | Homeboy binary path used by the service unit |
| `--user` | `<USER>` | System user that runs the broker service |
| `--group` | `<GROUP>` | System group that runs the broker service |
| `--domain` | `<DOMAIN>` | Optional public hostname to render disabled Nginx/Caddy examples |

## `homeboy daemon artifact-get`

```sh
homeboy daemon artifact-get [OPTIONS] <RUN_ID> <ARTIFACT_ID>
```

Fetch artifact bytes through the local daemon byte endpoint

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run id that owns the artifact |
| `<ARTIFACT_ID>` | yes | Artifact id/path token from daemon artifact metadata |

| Option | Value | Description |
| --- | --- | --- |
| `-o`, `--output` | `<OUTPUT>` | Destination file path. Defaults to the artifact id basename |
| `--daemon-url` | `<DAEMON_URL>` | Daemon base URL. Defaults to the address from `homeboy daemon status` |

