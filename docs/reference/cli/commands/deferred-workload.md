<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy deferred-workload` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/deferred-workload.md](../../../commands/deferred-workload.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy deferred-workload`

```sh
homeboy deferred-workload <COMMAND>
```

Resume portable workloads deferred until a runner is ready

| Subcommand | Summary |
| --- | --- |
| `homeboy deferred-workload worker` | Run the singleton controller-owned deferred-workload worker |
| `homeboy deferred-workload status` | Inspect deferred workloads and the controller worker |

## `homeboy deferred-workload worker`

```sh
homeboy deferred-workload worker [OPTIONS]
```

Run the singleton controller-owned deferred-workload worker

| Option | Value | Description |
| --- | --- | --- |
| `--startup-token` | `<TOKEN>` | _no help text_ |

## `homeboy deferred-workload status`

```sh
homeboy deferred-workload status
```

Inspect deferred workloads and the controller worker

