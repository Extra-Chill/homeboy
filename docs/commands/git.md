# `homeboy git`

## Synopsis

```sh
homeboy git <COMMAND>
```

## Subcommands

- `status <componentId>`
- `commit <componentId> <message>`
- `push <componentId> [--tags]`
- `pull <componentId>`
- `tag <componentId> [tagName] [-m <message>]`
  - If `tagName` is omitted, Homeboy tags `v<component version>` (from `homeboy version show`).

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "componentId": "<componentId>",
  "path": "<local path>",
  "action": "status|commit|push|pull|tag",
  "success": true,
  "exitCode": 0,
  "stdout": "<stdout>",
  "stderr": "<stderr>"
}
```

Notes:

- `commit` returns a successful result with `stdout` set to `Nothing to commit, working tree clean` when there are no changes.

## Exit code

Exit code matches the underlying `git` command.

## Related

- [version](version.md)
