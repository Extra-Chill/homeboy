# `homeboy build`

## Synopsis

```sh
homeboy build <componentId>
homeboy build --json <spec>
```

## Description

Runs a build command for the component in the component's `local_path`.

Requires `build_command` to be configured on the component. If not set, the command errors.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

### Single

```json
{
  "command": "build.run",
  "component_id": "<componentId>",
  "build_command": "<command string>",
  "stdout": "<stdout>",
  "stderr": "<stderr>",
  "success": true
}
```

### Bulk (`--json`)

```json
{
  "action": "build",
  "results": [
    {
      "id": "<componentId>",
      "result": {
        "command": "build.run",
        "component_id": "<componentId>",
        "build_command": "<command string>",
        "stdout": "<stdout>",
        "stderr": "<stderr>",
        "success": true
      },
      "error": null
    }
  ],
  "summary": { "total": 1, "succeeded": 1, "failed": 0 }
}
```

## Exit code

- Single mode: exit code matches the underlying build process exit code.
- Bulk mode (`--json`): `0` if all builds succeed; `1` if any build fails.

## Related

- [component](component.md)
- [deploy](deploy.md)
