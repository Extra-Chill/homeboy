<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy activity` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/activity.md](../../../commands/activity.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy activity`

```sh
homeboy activity [COMMAND]
```

Unified view of active and recently finished Homeboy work

| Subcommand | Summary |
| --- | --- |
| `homeboy activity list` | List active and recent Homeboy work |
| `homeboy activity show` | Resolve and show one activity item by run/task/job id |
| `homeboy activity watch` | Poll any activity item until it reaches a terminal state |

## `homeboy activity list`

```sh
homeboy activity list [OPTIONS]
```

List active and recent Homeboy work

| Option | Value | Description |
| --- | --- | --- |
| `--limit` | `<LIMIT>` | Maximum activity items to return |
| `--all` | flag | Include older completed records instead of active + recent |

## `homeboy activity show`

```sh
homeboy activity show <ID>
```

Resolve and show one activity item by run/task/job id

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | _no help text_ |

## `homeboy activity watch`

```sh
homeboy activity watch [OPTIONS] <ID>
```

Poll any activity item until it reaches a terminal state

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Activity id, observation run id, agent-task run id, or runner job id |

| Option | Value | Description |
| --- | --- | --- |
| `--timeout` | `<TIMEOUT>` | Maximum time to wait before giving up (e.g. `30m`, `2h`, `7d`) |
| `--interval` | `<INTERVAL>` | Delay between status polls (e.g. `2s`, `1m`) |
| `--notify` | flag | Emit a local completion notification when the item reaches a terminal state |

