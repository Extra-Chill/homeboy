# `homeboy api`

## Synopsis

```sh
homeboy api <projectId> <COMMAND>
```

## Description

Make HTTP requests to a project’s configured API.

This command uses the project’s API configuration (`projects/<projectId>.json`) and any stored authentication (see `homeboy auth`).

## Subcommands

### `get`

```sh
homeboy api <projectId> get <endpoint>
```

### `post`

```sh
homeboy api <projectId> post <endpoint> [--body <json>]
```

### `put`

```sh
homeboy api <projectId> put <endpoint> [--body <json>]
```

### `patch`

```sh
homeboy api <projectId> patch <endpoint> [--body <json>]
```

### `delete`

```sh
homeboy api <projectId> delete <endpoint>
```

## Notes

- `<endpoint>` is passed through as provided (example: `/wp/v2/posts`).
- `--body` is parsed as JSON. If parsing fails, the request is sent with `body: null`.

## Output

JSON output is wrapped in the global envelope. The `data.payload` is the `homeboy::api::ApiOutput` struct.

## Related

- [auth](auth.md)
- [project](project.md)
- [JSON output contract](../json-output/json-output-contract.md)
