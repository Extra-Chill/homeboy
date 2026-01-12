# `homeboy docs`

## Synopsis

```sh
homeboy docs [--list] [<topic>...]
```

## Description

By default, this command prints raw markdown to stdout (no JSON envelope).

Use `--list` to return a JSON list of available topics.

- Topic arguments are treated as a free-form trailing list.
- The resolved key must exist in embedded docs; otherwise the command errors.

Topic resolution is documented in: [Embedded docs topic resolution](../embedded-docs/embedded-docs-topic-resolution.md).

## Arguments

- `<topic>...` (optional): documentation topic. This must resolve to an embedded docs key (path under `docs/` without `.md`). Examples: `commands/deploy`, `commands/project`, `index`.

## Options

- `--list`: return available embedded keys and exit (JSON output).

## Output

### Default (render topic)

When `--list` is **not** used, `homeboy docs` writes the embedded markdown topic **as-is** to stdout (no JSON envelope, not wrapped).

### `--list`

When `--list` is used, output is JSON:

```json
{
  "success": true,
  "data": {
    "available_topics": ["index", "commands/deploy"],
    "mode": "list"
  }
}
```

## Errors

If resolved content is empty, the command returns an error message:

- `No documentation found for '<topic>' (available: <available_topics>)`

`<available_topics>` is a newline-separated list included in the error string.

## Related

- [Changelog command](changelog.md)
- [JSON output contract](../json-output/json-output-contract.md)
