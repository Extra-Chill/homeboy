# `homeboy wp`


## Synopsis

```sh
homeboy wp <projectId> [subtarget] <args...>
```

## Arguments and flags

- `projectId`: project ID
- `subtarget`: (optional) subtarget identifier for multisite projects
- `<args...>`: WP-CLI arguments (trailing var args; hyphen values allowed)

## Execution

Commands execute locally if the project has no `serverId` configured, or via SSH if `serverId` is set.

## Shell Quoting

Arguments are passed directly to WP-CLI. The shell processes quotes **before** homeboy receives them, so understanding quote behavior is essential.

### Do NOT quote multi-word commands

```sh
# WRONG - shell passes single arg: "datamachine-events health-check --scope=upcoming"
homeboy wp extra-chill events "datamachine-events health-check --scope=upcoming"

# CORRECT - shell passes separate args to homeboy
homeboy wp extra-chill events datamachine-events health-check --scope=upcoming
```

### DO quote values with spaces

```sh
# CORRECT - quotes protect the value, not the command structure
homeboy wp extra-chill post create --post_title="My New Post"
homeboy wp extra-chill user create bob@example.com --display_name="Bob Smith"
```

### Subtarget example

For projects with subtargets, specify the subtarget after the project ID:

```sh
# Run WP-CLI on the 'events' subtarget
homeboy wp extra-chill events core version

# Run a plugin command on a subtarget
homeboy wp extra-chill events datamachine-events health-check --scope=upcoming
```

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
