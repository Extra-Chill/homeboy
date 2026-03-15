# Hooks System

Homeboy provides a general-purpose hook/event system for lifecycle extensibility. Both components and extensions can declare hooks that run shell commands at named lifecycle events.

## Overview

Hooks are stored as a map of event names to command lists:

```json
{
  "hooks": {
    "pre:version:bump": ["cargo build --release"],
    "post:version:bump": ["git add Cargo.lock"],
    "post:release": ["curl -X POST https://hooks.example.com/done"]
  }
}
```

When an event fires, Homeboy resolves commands from two sources (in order):

1. **Extension hooks** — platform-level behavior from linked extensions
2. **Component hooks** — user-level customization

Commands execute sequentially in the component's `local_path` directory via `sh -c`.

## Events

| Event | When it runs | Failure mode |
|-------|-------------|--------------|
| `pre:version:bump` | After version targets are updated, before git commit | Fatal |
| `post:version:bump` | After pre-bump hooks, before git commit | Fatal |
| `post:release` | After the release pipeline completes | Non-fatal |
| `post:deploy` | After deploy completes | Non-fatal |

**Fatal** means a non-zero exit code aborts the operation. **Non-fatal** means failures are logged as warnings but the operation succeeds.

### `pre:version:bump`

Runs after version files are modified but before git commit. Use for building artifacts that include version info or staging generated files.

```json
{
  "hooks": {
    "pre:version:bump": [
      "cargo build --release",
      "npm run generate-schema"
    ]
  }
}
```

### `post:version:bump`

Runs after pre-bump hooks, still before git commit. Use for staging additional changed files or running post-bump validation.

```json
{
  "hooks": {
    "post:version:bump": [
      "git add Cargo.lock",
      "npm run format"
    ]
  }
}
```

### `post:release`

Runs after the release pipeline completes (all publish steps finished). Failures are non-fatal since the release is already published.

```json
{
  "hooks": {
    "post:release": [
      "curl -X POST https://hooks.example.com/release-complete",
      "rm -rf tmp/"
    ]
  }
}
```

### `post:deploy`

Runs after a successful deploy. Unlike other hooks, `post:deploy` hooks execute **remotely via SSH** on the deployment target, not locally. This enables post-deploy automation like plugin activation, cache flushing, or service restarts.

Template variables available in `post:deploy` hooks:

| Variable | Description |
|----------|-------------|
| `{{component_id}}` | The component ID |
| `{{install_dir}}` | Remote install directory (base_path + remote_path) |
| `{{base_path}}` | The project base path on the remote server |

```json
{
  "hooks": {
    "post:deploy": [
      "wp plugin activate {{component_id}} --path={{base_path}} --allow-root",
      "wp cache flush --path={{base_path}} --allow-root"
    ]
  }
}
```

Extension-level `post:deploy` hooks apply to all components using that extension. For example, the WordPress extension activates plugins and flushes cache after every deploy. Component-level hooks can add additional commands.

## Resolution Order

When hooks fire for an event, commands are collected in this order:

1. **Extension hooks** — iterate linked extensions, collect `hooks[event]` from each manifest
2. **Component hooks** — collect `hooks[event]` from the component config

Extension hooks run first so platform behavior executes before user customization.

## Execution Details

### Working Directory

Most hooks execute in the component's `local_path` directory via `sh -c`.

**Exception:** `post:deploy` hooks execute **remotely** on the deployment target via SSH. They do not have a working directory — use absolute paths or template variables like `{{base_path}}`.

### Command Format

Each command is a string passed to `sh -c`. Chain multiple operations with shell operators:

```json
{
  "hooks": {
    "post:version:bump": ["npm run lint && npm run test"]
  }
}
```

### Error Handling

For fatal events (`pre:version:bump`, `post:version:bump`):
- Non-zero exit code stops the operation immediately
- `stderr` output is included in the error message
- Remaining commands are skipped
- No automatic rollback of previous steps

For non-fatal events (`post:release`, `post:deploy`):
- Non-zero exit code logs a warning
- Remaining commands continue executing
- All results are captured in the operation output

## Backward Compatibility

Legacy flat fields (`pre_version_bump_commands`, `post_version_bump_commands`, `post_release_commands`) are still supported in component JSON. They are automatically migrated into the `hooks` map during deserialization:

```json
{
  "pre_version_bump_commands": ["cargo build --release"],
  "post_version_bump_commands": ["git add Cargo.lock"]
}
```

is equivalent to:

```json
{
  "hooks": {
    "pre:version:bump": ["cargo build --release"],
    "post:version:bump": ["git add Cargo.lock"]
  }
}
```

Both formats work. The `hooks` map is the canonical format going forward.

## Extension Hooks

Extensions declare hooks in their manifest using the same format:

```json
{
  "id": "rust",
  "hooks": {
    "post:version:bump": ["cargo generate-lockfile"]
  }
}
```

Extension hooks merge with component hooks at resolution time. They are not stored on the component.

## Hooks vs Release Pipeline Steps

| Feature | Hooks | Release Steps |
|---------|-------|---------------|
| Configuration | `hooks` map on component/extension | Release pipeline `steps` array |
| Dependencies | None (sequential) | `needs` field for DAG ordering |
| Failure handling | Fixed per event (fatal or non-fatal) | Configurable per step |
| Execution point | Fixed lifecycle points | Custom ordering |
| Use case | Simple shell commands | Complex orchestration |

**Use hooks** for simple, component-specific commands that always run at the same lifecycle point.

**Use release steps** for complex orchestration with dependencies, extension integration, or custom failure handling.

## Implementation

The hook engine lives in `src/core/engine/hooks.rs` and provides:

- `resolve_hooks(component, event)` — merge extension + component hooks for an event
- `run_hooks(component, event, failure_mode)` — resolve and execute locally
- `run_hooks_remote(ssh_client, component, event, failure_mode, vars)` — resolve, expand template variables, and execute via SSH
- `run_commands(commands, working_dir, event, failure_mode)` — low-level local executor
- `run_commands_remote(ssh_client, commands, event, failure_mode)` — low-level remote executor
- `events::*` — constants for standard event names
- `HookFailureMode` — `Fatal` or `NonFatal`
- `HookRunResult` / `HookCommandResult` — structured results

## Related

- [Release pipeline](release-pipeline.md) - Configurable release orchestration
- [Version command](../commands/version.md) - Version bump operations
