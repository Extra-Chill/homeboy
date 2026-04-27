# Execution Context

Execution context is the environment Homeboy passes to extension runners and extension-backed pipeline steps.

## Overview

When Homeboy executes an extension, it builds context from the resolved extension, optional project, optional component, and merged settings. The context is exposed as environment variables and template variables.

## Environment Variables

Homeboy sets these variables when the corresponding context exists:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: Execution context protocol version, currently `2`
- `HOMEBOY_EXTENSION_ID`: Extension identifier
- `HOMEBOY_EXTENSION_PATH`: Absolute path to the extension directory
- `HOMEBOY_SETTINGS_JSON`: Merged effective settings as JSON
- `HOMEBOY_PROJECT_ID`: Project identifier
- `HOMEBOY_PROJECT_PATH`: Absolute path to the project directory
- `HOMEBOY_COMPONENT_ID`: Component identifier
- `HOMEBOY_COMPONENT_PATH`: Absolute path to the component directory
- `HOMEBOY_STEP`: Comma-separated step filter
- `HOMEBOY_SKIP`: Comma-separated skip filter

## Context Resolution Flow

1. Extension is loaded from the installed extensions directory or an explicit development path.
2. Project context is resolved when the command or action asks for one.
3. Component context is resolved from an explicit component, project association, portable `homeboy.json`, or registered component metadata depending on the command.
4. Settings are merged from available scopes, with narrower scopes and CLI overrides winning.
5. Extension `runtime.env` values are resolved with template variables and added to the process environment.

## Template Variables

Common template variables include:

- `{{extensionPath}}`: Extension installation path
- `{{entrypoint}}`: Extension entrypoint from the manifest
- `{{args}}`: CLI arguments passed to the extension
- `{{projectId}}`: Project ID when project context exists
- `{{settings.<key>}}`: Merged setting value
- `{{payload.<key>}}`: Action payload data
- `{{release.<key>}}`: Release pipeline payload data

Project-specific extension manifests may define additional variables through their runtime configuration.

## Examples

### Component-Scoped Extension Execution

```bash
homeboy extension run rust --component mycomponent
```

Environment variables include:

```bash
HOMEBOY_EXEC_CONTEXT_VERSION=2
HOMEBOY_EXTENSION_ID=rust
HOMEBOY_EXTENSION_PATH=/home/user/.config/homeboy/extensions/rust
HOMEBOY_COMPONENT_ID=mycomponent
HOMEBOY_COMPONENT_PATH=/home/user/dev/mycomponent
HOMEBOY_SETTINGS_JSON={}
```

### Project and Component Context

```bash
homeboy extension run wordpress --project mysite --component mytheme
```

Environment variables include:

```bash
HOMEBOY_EXEC_CONTEXT_VERSION=2
HOMEBOY_EXTENSION_ID=wordpress
HOMEBOY_EXTENSION_PATH=/home/user/.config/homeboy/extensions/wordpress
HOMEBOY_PROJECT_ID=mysite
HOMEBOY_PROJECT_PATH=/home/user/projects/mysite
HOMEBOY_COMPONENT_ID=mytheme
HOMEBOY_COMPONENT_PATH=/home/user/dev/mytheme
HOMEBOY_SETTINGS_JSON={"php_version":"8.1"}
```

## Extension Environment Variables

Extensions can define additional environment variables in their manifest:

```json
{
  "runtime": {
    "run_command": "python3 {{entrypoint}} {{args}}",
    "env": {
      "PYTHON_PATH": "{{extensionPath}}/lib",
      "CACHE_DIR": "{{extensionPath}}/cache"
    }
  }
}
```

These are set alongside Homeboy's standard environment variables.

## Related

- [Extension command](../commands/extension.md)
- [Extension manifest schema](../schemas/extension-manifest-schema.md)
- [Template variables](../templates.md)
