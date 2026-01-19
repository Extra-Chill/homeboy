# `homeboy build`

## Synopsis

```sh
homeboy build <component_id>
homeboy build --json '<spec>'
```

## Description

Runs a build command for the component in the component's `local_path`.

Requires `build_command` to be configured on the component, or a module with build support. If neither is set, the command errors.

## Pre-Build Validation

If a component's module defines a `pre_build_script` in its build configuration, that script runs before the build. If the pre-build script exits with a non-zero code, the build fails.

For WordPress components, this runs PHP syntax validation to catch errors before building.

Example module configuration:
```json
{
  "build": {
    "script_names": ["build.sh"],
    "module_script": "scripts/build.sh",
    "pre_build_script": "scripts/validate-build.sh"
  }
}
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

### Single

```json
{
  "command": "build.run",
  "component_id": "<component_id>",
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
      "id": "<component_id>",
      "result": {
        "command": "build.run",
        "component_id": "<component_id>",
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

Bulk JSON input uses `component_ids`:

```json
{ "component_ids": ["component-a", "component-b"] }
```

## Exit code

- Single mode: exit code matches the underlying build process exit code.
- Bulk mode (`--json`): `0` if all builds succeed; `1` if any build fails.

## Related

- [component](component.md)
- [deploy](deploy.md)
- [lint](lint.md)
- [test](test.md)
