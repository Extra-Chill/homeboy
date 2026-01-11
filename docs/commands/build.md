# `homeboy build`

## Synopsis

```sh
homeboy build <component_id>
```

## Description

Runs the component’s configured `build_command` in the component’s `local_path`.

## JSON output

```json
{
  "command": "build",
  "componentId": "<id>",
  "buildCommand": "<command string>",
  "stdout": "<stdout>",
  "stderr": "<stderr>",
  "success": true
}
```

## Exit code

Exit code matches the build command process exit code.

## Related

- [component](component.md)
- [deploy](deploy.md)
