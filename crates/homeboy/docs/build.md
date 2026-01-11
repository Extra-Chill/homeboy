# build

Build a component using its configured build command.

```bash
homeboy build <component-id>
```

## Usage

Runs the component's configured `build_command` in its `local_path`. The build command is executed via `sh -c`, so any shell command is supported.

```bash
homeboy build my-component
```

## Requirements

- Component must have `build_command` configured.
- The component's `local_path` must be accessible.

## Output

The command returns JSON output containing:
- `command`: The command that was executed
- `component_id`: The component identifier
- `build_command`: The build command that was run
- `stdout`: Standard output from the build
- `stderr`: Standard error from the build
- `success`: Whether the build succeeded
