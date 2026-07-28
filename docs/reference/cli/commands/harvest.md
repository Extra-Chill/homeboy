<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy harvest` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/harvest.md](../../../commands/harvest.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy harvest`

```sh
homeboy harvest [OPTIONS] <TARGET_ID> [COMPONENT_IDS]...
```

Recover remote component content into local Git history

| Argument | Required | Description |
| --- | --- | --- |
| `<TARGET_ID>` | yes | Project ID or component ID (order is auto-detected) |
| `[COMPONENT_IDS]...` | no | Additional component IDs or the project ID |

| Option | Value | Description |
| --- | --- | --- |
| `--check` | flag | Report remote content drift without writing local files |
| `--dry-run` | flag | Print the remote content delta without writing or committing |
| `--apply` | flag | Materialize the reviewed remote delta and commit it |
| `--exclude` | `<EXCLUDE>` | Relative glob to exclude. Repeat for multiple patterns |
| `--author` | `<AUTHOR>` | Git author for the recovery commit, for example 'Remote agent <agent@example.invalid>' |

