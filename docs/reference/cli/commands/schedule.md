<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy schedule` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/schedule.md](../../../commands/schedule.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy schedule`

```sh
homeboy schedule <COMMAND>
```

Declare homeboy commands that run on a cadence

| Subcommand | Summary |
| --- | --- |
| `homeboy schedule add` | Declare a scheduled run |
| `homeboy schedule list` | List declared schedules with their last and next run |
| `homeboy schedule show` | Show one schedule and its runtime state |
| `homeboy schedule remove` | Remove a schedule and its runtime state |
| `homeboy schedule run` | Run a schedule now, regardless of whether it is due |
| `homeboy schedule enable` | Enable a disabled schedule |
| `homeboy schedule disable` | Disable a schedule without deleting it |
| `homeboy schedule tick` | Run every schedule that is currently due |

## `homeboy schedule add`

```sh
homeboy schedule add [OPTIONS] <ID>
```

Declare a scheduled run

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Schedule id |

| Option | Value | Description |
| --- | --- | --- |
| `--command` | `<COMMAND>` | Homeboy command to run, without the leading binary name (for example: --command "fleet check prod") |
| `--exec` | `<EXEC>` | External program to run. Executed directly, never through a shell |
| `--exec-arg` | `<EXEC_ARG>` | Argument for the preceding --exec. Repeat for each argument; values are passed through untouched, so an argument may contain spaces |
| `--working-dir` | `<WORKING_DIR>` | Directory to run the preceding --exec from |
| `--every` | `<EVERY>` | How often to run: 30m, 24h, 1h30m, 7d |
| `--notify-on` | `<NOTIFY_ON>` | When to notify: change (default), failure, or always |
| `--on-overlap` | `<ON_OVERLAP>` | What to do if the previous run is still going: skip (default) or allow |
| `--notification-transport` | `<NOTIFICATION_TRANSPORT>` | Notification transport id (requires --notification-route) |
| `--notification-route` | `<NOTIFICATION_ROUTE>` | Notification route (requires --notification-transport) |
| `--jitter-seconds` | `<JITTER_SECONDS>` | Spread runs across a window, in seconds, so many schedules sharing a cadence do not fire at the same instant |
| `--description` | `<DESCRIPTION>` | Human-readable note about why this schedule exists |
| `--force` | flag | Replace an existing schedule with the same id |

## `homeboy schedule list`

```sh
homeboy schedule list
```

List declared schedules with their last and next run

## `homeboy schedule show`

```sh
homeboy schedule show <ID>
```

Show one schedule and its runtime state

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | _no help text_ |

## `homeboy schedule remove`

```sh
homeboy schedule remove <ID>
```

Remove a schedule and its runtime state

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | _no help text_ |

## `homeboy schedule run`

```sh
homeboy schedule run <ID>
```

Run a schedule now, regardless of whether it is due

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | _no help text_ |

## `homeboy schedule enable`

```sh
homeboy schedule enable <ID>
```

Enable a disabled schedule

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | _no help text_ |

## `homeboy schedule disable`

```sh
homeboy schedule disable <ID>
```

Disable a schedule without deleting it

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | _no help text_ |

## `homeboy schedule tick`

```sh
homeboy schedule tick [OPTIONS]
```

Run every schedule that is currently due

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Report what is due without running anything |
