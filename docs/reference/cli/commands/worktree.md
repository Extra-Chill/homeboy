<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy worktree` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/worktree.md](../../../commands/worktree.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy worktree`

```sh
homeboy worktree <COMMAND>
```

Manage component-backed task worktrees

| Subcommand | Summary |
| --- | --- |
| `homeboy worktree create` | Create a task worktree through Homeboy's native lifecycle registry |
| `homeboy worktree import` | Import an existing exact Git worktree into the built-in lifecycle registry |
| `homeboy worktree finalize` | Record a terminal worktree disposition without performing cleanup |
| `homeboy worktree adopt` | Adopt an existing local workspace path for @workspace:<handle> refs |
| `homeboy worktree queue-create` | Create multiple task worktrees one-at-a-time with queue status JSON |
| `homeboy worktree list` | List task worktrees registered with Homeboy |
| `homeboy worktree inventory` | Report bounded local task-worktree inventory and reconcile only leased terminal snapshots |
| `homeboy worktree status` | Inspect a native task worktree and its safety state |
| `homeboy worktree holder` | Report the session currently holding a managed checkout's write lease |
| `homeboy worktree remove` | Remove one task worktree after safety checks |
| `homeboy worktree cleanup` | Clean up eligible native task worktrees |
| `homeboy worktree quarantine` | Inspect or explicitly reconcile quarantined malformed task-worktree records |

## `homeboy worktree create`

```sh
homeboy worktree create [OPTIONS] <COMPONENT_ID>
```

Create a task worktree through Homeboy's native lifecycle registry

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component or repository handle for native creation |

| Option | Value | Description |
| --- | --- | --- |
| `--branch` | `<BRANCH>` | Branch to create in the task worktree |
| `--from` | `<FROM>` | Base ref for the new worktree branch |
| `--task-url` | `<TASK_URL>` | Task or issue URL associated with this worktree |
| `--run-id` | `<RUN_ID>` | Agent-task run ID associated with this worktree |
| `--cleanup-policy` | `<CLEANUP_POLICY>` | Cleanup policy for lifecycle cleanup Values: `remove-when-safe`, `preserve-on-failure`. |

## `homeboy worktree import`

```sh
homeboy worktree import [OPTIONS] <COMPONENT_ID> <HANDLE> <PATH>
```

Import an existing exact Git worktree into the built-in lifecycle registry

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | _no help text_ |
| `<HANDLE>` | yes | _no help text_ |
| `<PATH>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--branch` | `<BRANCH>` | _no help text_ |
| `--base-ref` | `<BASE_REF>` | _no help text_ |
| `--task-url` | `<TASK_URL>` | _no help text_ |
| `--owner-run-ref` | `<OWNER_RUN_REF>` | _no help text_ |
| `--cleanup-policy` | `<CLEANUP_POLICY>` | _no help text_ Values: `remove-when-safe`, `preserve-on-failure`. |
| `--created-at` | `<CREATED_AT>` | _no help text_ |

## `homeboy worktree finalize`

```sh
homeboy worktree finalize [OPTIONS] <HANDLE>
```

Record a terminal worktree disposition without performing cleanup

| Argument | Required | Description |
| --- | --- | --- |
| `<HANDLE>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--owner-run-ref` | `<OWNER_RUN_REF>` | _no help text_ |
| `--disposition` | `<DISPOSITION>` | _no help text_ Values: `succeeded`, `failed`, `cancelled`, `timed-out`, `interrupted`. |

## `homeboy worktree adopt`

```sh
homeboy worktree adopt [OPTIONS] <HANDLE> <PATH>
```

Adopt an existing local workspace path for @workspace:<handle> refs

| Argument | Required | Description |
| --- | --- | --- |
| `<HANDLE>` | yes | Workspace handle resolved by @workspace:<handle> |
| `<PATH>` | yes | Existing local directory to resolve for this handle |

| Option | Value | Description |
| --- | --- | --- |
| `--kind` | `<KIND>` | Optional generic kind label recorded as provenance |
| `--provenance-json` | `<PROVENANCE_JSON>` | Optional JSON provenance payload recorded with the adopted path |

## `homeboy worktree queue-create`

```sh
homeboy worktree queue-create [OPTIONS] <REPO>
```

Create multiple task worktrees one-at-a-time with queue status JSON

| Argument | Required | Description |
| --- | --- | --- |
| `<REPO>` | yes | Registered component/repo handle, e.g. homeboy |

| Option | Value | Description |
| --- | --- | --- |
| `--branch` | `<BRANCH>` | Branch to create. Repeat for fanout batches |
| `--from` | `<FROM>` | Base ref for each worktree branch |
| `--task-url` | `<TASK_URL>` | Task or issue URL associated with these worktrees |
| `--task-ref` | `<TASK_REF>` | Short task reference associated with these worktrees, e.g. Extra-Chill/homeboy#5786 |
| `--dry-run` | flag | Print the queue plan/status without creating worktrees |
| `--retry-after-seconds` | `<RETRY_AFTER_SECONDS>` | Suggested orchestrator wait when queueing is blocked but no retry-after value is available |

## `homeboy worktree list`

```sh
homeboy worktree list
```

List task worktrees registered with Homeboy

## `homeboy worktree inventory`

```sh
homeboy worktree inventory [OPTIONS]
```

Report bounded local task-worktree inventory and reconcile only leased terminal snapshots

| Option | Value | Description |
| --- | --- | --- |
| `--limit` | `<LIMIT>` | Maximum task-worktree manifests to inspect |
| `--cursor` | `<CURSOR>` | Start after this task-worktree record ID |
| `--adopted-cursor` | `<ADOPTED_CURSOR>` | Start after this adopted-workspace handle |
| `--apply` | flag | Conditionally reconcile clean, missing worktrees with terminal authority; preserve or refuse all other records |

## `homeboy worktree status`

```sh
homeboy worktree status <ID>
```

Inspect a native task worktree and its safety state

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Task worktree ID, e.g. component@branch-slug |

## `homeboy worktree holder`

```sh
homeboy worktree holder <TARGET>
```

Report the session currently holding a managed checkout's write lease

| Argument | Required | Description |
| --- | --- | --- |
| `<TARGET>` | yes | Managed worktree handle or any path inside the checkout |

## `homeboy worktree remove`

```sh
homeboy worktree remove [OPTIONS] <ID>
```

Remove one task worktree after safety checks

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Task worktree ID, e.g. component@branch-slug |

| Option | Value | Description |
| --- | --- | --- |
| `--force` | flag | Allow dirty/unpushed worktree removal; hard gates still apply |
| `--cleanup-branch` | flag | Delete the local task branch after removing the worktree when branch safety allows it |
| `--allow-unmerged-branch` | flag | Permit deleting an unmerged task branch. Requires --cleanup-branch |

## `homeboy worktree cleanup`

```sh
homeboy worktree cleanup [OPTIONS]
```

Clean up eligible native task worktrees

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Execute the mutation. Without this flag the command reports a plan only |
| `--dry-run` | flag | Explicitly request the plan-only default. Never mutates |
| `--force` | flag | Allow dirty/unpushed worktree removal; hard gates still apply |
| `--cleanup-artifacts` | flag | Also remove declared rebuildable artifacts from the Homeboy checkout that built this binary |
| `--cleanup-branches` | flag | Delete merged task branches for removed cleanup candidates |
| `--allow-unmerged-branches` | flag | Permit deleting unmerged task branches. Requires --cleanup-branches |

## `homeboy worktree quarantine`

```sh
homeboy worktree quarantine <COMMAND>
```

Inspect or explicitly reconcile quarantined malformed task-worktree records

| Subcommand | Summary |
| --- | --- |
| `homeboy worktree quarantine list` | List quarantined records still protecting Cargo targets |
| `homeboy worktree quarantine clear` | Mark one quarantined record terminally reconciled while retaining its original evidence |

## `homeboy worktree quarantine list`

```sh
homeboy worktree quarantine list
```

List quarantined records still protecting Cargo targets

## `homeboy worktree quarantine clear`

```sh
homeboy worktree quarantine clear [OPTIONS] <PROVENANCE_PATH>
```

Mark one quarantined record terminally reconciled while retaining its original evidence

| Argument | Required | Description |
| --- | --- | --- |
| `<PROVENANCE_PATH>` | yes | Provenance sidecar reported by cleanup or `worktree quarantine list` |

| Option | Value | Description |
| --- | --- | --- |
| `--verified-terminal` | flag | Confirms terminal state was independently verified before clearing protection |
