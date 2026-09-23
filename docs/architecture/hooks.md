# Hooks System

Homeboy provides a general-purpose hook/event system for lifecycle extensibility. Extensions, deploy targets (projects), and components can all declare hooks that run shell commands at named lifecycle events.

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

For a **component-scoped** event (e.g. `post:deploy`), Homeboy resolves commands from three sources, in order:

1. **Extension hooks** — platform-level behavior from linked extensions
2. **Project hooks** — site-level policy, declared on the deploy target itself (`Project::hooks`)
3. **Component hooks** — user-level customization

Commands execute sequentially in the component's `local_path` directory via `sh -c`.

`post:deploy:project` is a different shape: it is **deploy-target-scoped**, not component-scoped. It is declared only on the project's `hooks` map and is never merged from extensions or components — see [Deploy-Target-Scoped Hooks](#deploy-target-scoped-hooks-post-deployproject) below.

## Events

| Event | Scope | When it runs | Failure mode |
|-------|-------|-------------|--------------|
| `pre:version:bump` | Component | After version targets are updated, before git commit | Fatal |
| `post:version:bump` | Component | After pre-bump hooks, before git commit | Fatal |
| `post:release` | Component | After the release pipeline completes | Non-fatal |
| `post:deploy` | Component | After each component's deploy completes (once per component) | Non-fatal |
| `post:deploy:project` | Deploy target (project) | Once per deploy invocation, after the last component | Non-fatal |

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

### Project-level `post:deploy` hooks

A project can also declare `post:deploy` in its own `hooks` map (`Project::hooks`, distinct from `component_overrides[id].hooks`). These merge into every component deployed to that project, between extension hooks and component hooks — see [Resolution Order](#resolution-order). This is for a step that is a property of *this project's* components in general (e.g. every plugin on this site should also run a per-plugin sanity check), not a step scoped to the deploy invocation as a whole.

### Deploy-target-scoped hooks: `post:deploy:project`

`post:deploy:project` runs **once per deploy invocation**, after every component has finished deploying — not once per component. It exists for a step that belongs to *where* you deploy, not to any one component or platform: the canonical example is a site-wide page-cache purge, which only needs to run once even when a deploy touches ten plugins.

It is declared only on the project:

```json
{
  "id": "extrachill-site",
  "hooks": {
    "post:deploy:project": [
      "wp extrachill-cache purge --all --path={{base_path}} --allow-root"
    ]
  }
}
```

Unlike `post:deploy`, this event is **never merged from extensions or components** — only `Project::hooks` is consulted. This is deliberate: routing a site-specific step like a cache purge through a generic, vendor-neutral extension is exactly the layering problem this event exists to avoid (a generic WordPress extension should not name every site's cache plugin). If a step genuinely belongs to a component or a platform, it belongs in `post:deploy`/extension hooks instead, not here.

Runs only when at least one component actually deployed. Non-fatal, remote via SSH, and expands the same `{{base_path}}` template variable as `post:deploy` (plus `{{projectId}}`); `{{component_id}}` and `{{install_dir}}` are not available since the event is not tied to one component.

## Resolution Order

For a **component-scoped** event (`post:deploy`), commands are collected in this order:

1. **Extension hooks** — iterate linked extensions, collect `hooks[event]` from each manifest
2. **Project hooks** — collect `hooks[event]` from the project's own `Project::hooks`
3. **Component hooks** — collect `hooks[event]` from the resolved component config

Extension hooks run first so platform behavior executes before site policy, which runs before user/component customization.

For the **deploy-target-scoped** event (`post:deploy:project`), there is no merge: only the project's `Project::hooks[event]` runs, once, after every component in the deploy has finished. Extensions and components cannot declare it.

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

The hook engine lives in `crates/homeboy-core/src/engine/hooks.rs` and provides:

- `resolve_hooks(component, event)` — merge extension + component hooks for an event (no project layer)
- `resolve_hooks_with_project(component, project_hooks, event)` — merge extension + project + component hooks for an event
- `run_hooks(component, event, failure_mode)` — resolve and execute locally (no project layer)
- `run_hooks_remote(ssh_client, component, event, failure_mode, vars)` — resolve (no project layer), expand template variables, and execute via SSH
- `run_hooks_remote_with_project(ssh_client, component, project_hooks, event, failure_mode, vars)` — resolve with the project layer, expand template variables, and execute via SSH; used for `post:deploy`
- `run_project_scoped_hooks_remote(ssh_client, target_hooks, event, failure_mode, vars)` — read only a deploy target's own hook map (no extension/component merge), expand template variables, and execute via SSH; used for `post:deploy:project`
- `run_commands(commands, working_dir, event, failure_mode)` — low-level local executor
- `run_commands_remote(ssh_client, commands, event, failure_mode)` — low-level remote executor
- `events::*` — constants for standard event names
- `HookFailureMode` — `Fatal` or `NonFatal`
- `HookRunResult` / `HookCommandResult` — structured results

The `post:deploy:project` call site — deciding whether anything deployed and invoking the hook exactly once per deploy run — lives in `crates/homeboy-deploy/src/orchestration/project_hooks.rs`.

## Related

- [Release pipeline](release-pipeline.md) - Configurable release orchestration
- [Version command](../commands/version.md) - Version bump operations
