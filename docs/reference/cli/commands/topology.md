<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy topology` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/topology.md](../../../commands/topology.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy topology`

```sh
homeboy topology <KIND> <ID>
```

Inspect declared resource relationships without resolving effective configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<KIND>` | yes | Kind of the root resource to inspect Values: `component`, `project`, `server`, `fleet`, `runner`. |
| `<ID>` | yes | ID of the root resource to inspect |
