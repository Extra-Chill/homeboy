# `homeboy wp`


## Synopsis

```sh
homeboy wp <projectId> <args...>
```

## Arguments and flags

- `projectId`: project ID
- `<args...>`: CLI tool arguments (trailing var args; hyphen values allowed)

## Execution

Commands execute locally if the project has no `serverId` configured, or via SSH if `serverId` is set.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "projectId": "<projectId>",
  "args": ["core", "version"],
  "targetDomain": "<domain>|null",
  "command": "<rendered command string>",
  "stdout": "<stdout>",
  "stderr": "<stderr>",
  "exitCode": 0
}
```

Notes:

- The command errors if no args are provided.
- For projects with subtargets, the first arg may be a subtarget identifier. Matching prefers `subtarget.slug_id()` (and falls back to identifier/name matching); `targetDomain` reflects the resolved domain.

## Exit code

Exit code matches the executed CLI tool command.

## Related

- [module](module.md)
- [db](db.md)
