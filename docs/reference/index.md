# Reference

Reference docs describe exact command behavior, configuration fields, schemas, templates, and output contracts. Use workflows first when you need a task path.

Reference pages are intentionally dense. If you are deciding what to run next, start with [Workflows](../workflows/index.md); if you already know the command or config surface, use this section.

## CLI

- [Generated CLI reference](cli/commands/index.md) - every command, argument, flag, and subcommand, generated from clap
- [Root command and global flags](cli/homeboy-root-command.md)
- [Command index](../commands/commands-index.md) - hand-written narrative per command family
- [Contract manifest command](../commands/contract.md)

## Configuration

- [Configuration reference](configuration.md)
- [Template variables](template-variables.md)
- [Configuration schemas](schemas/index.md)

## Output And Contracts

- [JSON output contract](../architecture/output-system.md)
- [CI result JSON contract](../architecture/ci-results-contract.md)
- [Structured sidecars](../architecture/structured-sidecars.md)

## Extension Commands

Extensions add top-level commands at runtime, so they are not part of the
clap-generated reference above.

- [Cargo command](../commands/cargo.md)
