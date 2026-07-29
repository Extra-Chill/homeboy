<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
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
| `homeboy worktree create` | Create a task worktree from a registered component checkout |
| `homeboy worktree adopt` | Adopt an existing local workspace path for @workspace:<handle> refs |
| `homeboy worktree queue-create` | Create multiple task worktrees one-at-a-time with queue status JSON |
| `homeboy worktree list` | List persisted task worktrees |
| `homeboy worktree status` | Inspect one task worktree and its safety gates |
| `homeboy worktree remove` | Remove one task worktree after safety checks |
| `homeboy worktree cleanup` | Remove cleanup-eligible task worktrees after safety checks |
| `homeboy worktree quarantine` | Inspect or explicitly reconcile quarantined malformed task-worktree records |

## `homeboy worktree create`

```sh
homeboy worktree create [OPTIONS] <COMPONENT_ID>
```

Create a task worktree from a registered component checkout

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID to use as the source checkout |

| Option | Value | Description |
| --- | --- | --- |
| `--branch` | `<BRANCH>` | Branch to create in the task worktree |
| `--from` | `<FROM>` | Base ref for the new worktree branch |
| `--task-url` | `<TASK_URL>` | Task or issue URL associated with this worktree |
| `--run-id` | `<RUN_ID>` | Agent-task run ID associated with this worktree |
| `--cleanup-policy` | `<CLEANUP_POLICY>` | Cleanup policy for lifecycle cleanup Values: `remove-when-safe`, `preserve-on-failure`. |

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

List persisted task worktrees

## `homeboy worktree status`

```sh
homeboy worktree status <ID>
```

Inspect one task worktree and its safety gates

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Task worktree ID, e.g. component@branch-slug |

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

Remove cleanup-eligible task worktrees after safety checks

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Remove planned worktrees and artifacts after safety checks. Without this flag, only reports the plan |
| `--force` | flag | Allow dirty/unpushed worktree removal; hard gates still apply |
| `--dry-run` | flag | Deprecated plan-only alias retained for one release; bare cleanup also reports the plan |
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
