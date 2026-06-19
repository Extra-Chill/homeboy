# `homeboy list`

## Synopsis

```sh
homeboy list
```

## Description

`homeboy list` is a convenience command that prints the same help text as `homeboy --help`.

This command is retained as a compatibility alias for existing operators and scripts. Prefer `homeboy --help` for new usage.

`homeboy list` is intentionally raw help output rather than a structured command surface API.

## Output

This command prints clap-generated `homeboy --help` output to stdout.

- Output is help-text (no JSON envelope).

## Exit code

- `0` on success.

## Related

- [Root command](../cli/homeboy-root-command.md)
- [Commands index](commands-index.md)
