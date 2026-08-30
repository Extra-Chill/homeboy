<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy upgrade` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/upgrade.md](../../../commands/upgrade.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy upgrade`

```sh
homeboy upgrade [OPTIONS] [COMMAND]
```

Upgrade Homeboy to the latest version

| Option | Value | Description |
| --- | --- | --- |
| `--check` | flag | Check for updates without installing |
| `--force` | flag | Force upgrade even if already at latest version |
| `--skip-extensions` | flag | Skip extension updates (only upgrade the binary) |
| `--skip-runners` | flag | Skip configured runner upgrades after the local upgrade |
| `--no-restart-services` | flag | Skip restarting declared binary-resident services after the binary swap. They will be reported as pending with their recovery commands instead |
| `--upgrade-runner` | `<RUNNER_ID>` | Select the configured runner to converge with the controller. Repeat to target multiple runners |
| `--runner-only` | flag | Refresh selected runners without promoting the controller |
| `--method` | `<METHOD>` | Override install method detection (homebrew\|cargo\|source\|binary) |
| `--source-path` | `<PATH>` | Homeboy source checkout to use with --method source |
| `--version` | `<TAG>` | Pin a published release tag; infers --method binary when omitted |

| Subcommand | Summary |
| --- | --- |
| `homeboy upgrade status` | Inspect a persisted upgrade operation |

## `homeboy upgrade status`

```sh
homeboy upgrade status [ID]
```

Inspect a persisted upgrade operation

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Operation id from a previous `homeboy upgrade`. Defaults to the latest upgrade run |
