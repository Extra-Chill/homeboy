<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy observe` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/observe.md](../../../commands/observe.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy observe`

```sh
homeboy observe [OPTIONS] [COMPONENT]
```

Passively observe a running system and persist timeline evidence

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--duration` | `<DURATION>` | _no help text_ |
| `--tail-log` | `<PATH>` | _no help text_ |
| `--grep` | `<REGEX>` | _no help text_ |
| `--watch-process` | `<REGEX>` | _no help text_ |
| `--watch-process-interval` | `<WATCH_PROCESS_INTERVAL>` | _no help text_ |
| `--probe` | `<JSON>` | _no help text_ |

