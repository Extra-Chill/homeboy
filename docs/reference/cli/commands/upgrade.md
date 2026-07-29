<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy upgrade` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/upgrade.md](../../../commands/upgrade.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy upgrade`

```sh
homeboy upgrade [OPTIONS]
```

Upgrade Homeboy to the latest version

| Option | Value | Description |
| --- | --- | --- |
| `--check` | flag | Check for updates without installing |
| `--force` | flag | Force upgrade even if already at latest version |
| `--no-restart` | flag | Skip automatic restart after upgrade |
| `--skip-extensions` | flag | Skip extension updates (only upgrade the binary) |
| `--skip-runners` | flag | Skip configured runner upgrades after the local upgrade |
| `--no-restart-services` | flag | Skip restarting declared binary-resident services after the binary swap. They will be reported as pending with their recovery commands instead |
| `--upgrade-runner` | `<RUNNER_ID>` | Select the configured runner to converge with the controller. Repeat to target multiple runners |
| `--runner-only` | flag | Refresh selected runners without promoting the controller |
| `--method` | `<METHOD>` | Override install method detection (homebrew\|cargo\|source\|binary) |
| `--source-path` | `<PATH>` | Homeboy source checkout to use with --method source |
