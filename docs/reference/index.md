# Reference

Reference docs describe exact command behavior, configuration fields, schemas, templates, and output contracts. Use workflows first when you need a task path.

Reference pages are intentionally dense. If you are deciding what to run next, start with [Workflows](../workflows/index.md); if you already know the command or config surface, use this section.

## CLI

- [Root command and global flags](cli/homeboy-root-command.md)
- [Command concepts and workflows](../commands/index.md)
- Runtime command index: `homeboy self docs commands/commands-index`
- Exact command help: `homeboy <command> --help`
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

Extensions add top-level commands at runtime. They appear automatically in
`homeboy --help` and the runtime command index when installed.

- [Cargo command](../commands/cargo.md)
