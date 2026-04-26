# Release Pipeline System

The release pipeline provides configurable, local orchestration for managing component releases without CI/CD systems.

## Overview

Homeboy's release pipeline is a local-first alternative to CI/CD, allowing developers to:
- Define release workflows as configuration
- Plan and review releases before execution
- Integrate custom extension actions
- Run releases from local development environment

## Pipeline Configuration

Release pipelines are defined in component configuration:

```json
{
  "release": {
    "enabled": true,
    "steps": [],
    "settings": {}
  }
}
```

### Release Fields

- **`enabled`** (boolean): Whether release pipeline is active
- **`steps`** (array): Ordered list of release steps
- **`settings`** (object): Pipeline-level settings

## Step Types

### Built-in Step Types

#### `build`

Build the component using its linked extension's build support.

```json
{
  "id": "build",
  "type": "build",
  "label": "Build",
  "needs": [],
  "config": {}
}
```

**Config options:** None (uses linked extension build configuration)

#### `version_bump`

Increment component version.

```json
{
  "id": "bump",
  "type": "version_bump",
  "label": "Bump Version",
  "needs": [],
  "config": {
    "bump_type": "patch|minor|major"
  }
}
```

**Config options:**
- **`bump_type`** (string): Version increment type (`patch`, `minor`, `major`)

#### `git_commit`

Create a git commit.

```json
{
  "id": "commit",
  "type": "git_commit",
  "label": "Commit Changes",
  "needs": ["build"],
  "config": {
    "message": "Release {version}",
    "add_all": false,
    "add_staged": true
  }
}
```

**Config options:**
- **`message`** (string): Commit message (supports `{{version}}` variable)
- **`add_all`** (boolean): Stage all changes before committing
- **`add_staged`** (boolean): Commit only staged changes

#### `git_tag`

Create a git tag for the version.

```json
{
  "id": "tag",
  "type": "git_tag",
  "label": "Create Tag",
  "needs": ["bump"],
  "config": {
    "tag_format": "v{version}",
    "push": true
  }
}
```

**Config options:**
- **`tag_format`** (string): Tag format string (supports `{{version}}`)
- **`push`** (boolean): Push tags to remote

#### `git_push`

Push commits and tags to remote.

```json
{
  "id": "push",
  "type": "git_push",
  "label": "Push to Remote",
  "needs": ["tag"],
  "config": {
    "push_tags": true
  }
}
```

**Config options:**
- **`push_tags`** (boolean): Push tags along with commits

#### `extension_run`

Execute a extension runtime command.

```json
{
  "id": "test",
  "type": "extension_run",
  "label": "Run Tests",
  "needs": ["build"],
  "config": {
    "extension": "rust",
    "command": "test",
    "inputs": [
      {"id": "release", "value": "true"}
    ]
  }
}
```

**Config options:**
- **`extension`** (string): Extension ID to execute
- **`command`** (string): Command to pass to extension
- **`inputs`** (array): Input arguments for extension

#### `extension_action`

Execute a extension action.

```json
{
  "id": "publish",
  "type": "extension_action",
  "label": "Publish Release",
  "needs": ["push"],
  "config": {
    "extension": "github",
    "action": "create_release",
    "data": {}
  }
}
```

**Config options:**
- **`extension`** (string): Extension ID providing the action
- **`action`** (string): Action ID to execute
- **`data`** (object): Data to pass to action

## Step Dependencies

The `needs` field defines step execution order:

```json
{
  "steps": [
    {"id": "test", "type": "extension_run", "needs": []},
    {"id": "build", "type": "build", "needs": []},
    {"id": "bump", "type": "version_bump", "needs": ["test", "build"]},
    {"id": "commit", "type": "git_commit", "needs": ["bump"]},
    {"id": "push", "type": "git_push", "needs": ["commit"]}
  ]
}
```

Steps with empty `needs` arrays run first. Steps wait for all dependencies to complete successfully before executing.

## Extension Actions in Pipelines

Extensions can define `release_actions` in their manifest for pipeline integration:

```json
{
  "release_actions": {
    "publish": {
      "type": "extension_run",
      "config": {
        "extension": "github",
        "inputs": [
          {"id": "create_release", "value": "true"}
        ]
      }
    }
  }
}
```

These can be referenced in pipeline steps:

```json
{
  "id": "publish",
  "type": "extension.run",
  "label": "Publish",
  "needs": ["push"],
  "config": {}
}
```

## Pipeline Commands

### Plan Release

Review what a release pipeline will do without executing:

```bash
homeboy release plan <component_id>
```

Shows:
- Pipeline steps in execution order
- Dependency graph
- Current version
- Next version after bump
- Configuration validation

### Run Release

Execute the release pipeline:

```bash
homeboy release run <component_id>
```

Execution:
1. Validates pipeline configuration
2. Executes steps in dependency order
3. Stops on first failure
4. Reports status for each step

## Complete Example

```json
{
  "release": {
    "enabled": true,
    "steps": [
      {
        "id": "lint",
        "type": "extension_run",
        "label": "Lint Code",
        "needs": [],
        "config": {
          "extension": "rust",
          "command": "clippy"
        }
      },
      {
        "id": "test",
        "type": "extension_run",
        "label": "Run Tests",
        "needs": [],
        "config": {
          "extension": "rust",
          "command": "test"
        }
      },
      {
        "id": "build",
        "type": "build",
        "label": "Build Artifact",
        "needs": ["lint", "test"],
        "config": {}
      },
      {
        "id": "bump",
        "type": "version_bump",
        "label": "Bump Version",
        "needs": ["build"],
        "config": {
          "bump_type": "patch"
        }
      },
      {
        "id": "commit",
        "type": "git_commit",
        "label": "Commit Release",
        "needs": ["bump"],
        "config": {
          "message": "Release {{version}}",
          "add_staged": true
        }
      },
      {
        "id": "tag",
        "type": "git_tag",
        "label": "Create Tag",
        "needs": ["bump"],
        "config": {
          "tag_format": "v{{version}}",
          "push": true
        }
      },
      {
        "id": "push",
        "type": "git_push",
        "label": "Push to Remote",
        "needs": ["commit", "tag"],
        "config": {
          "push_tags": true
        }
      },
      {
        "id": "publish",
        "type": "extension_action",
        "label": "Create GitHub Release",
        "needs": ["push"],
        "config": {
          "extension": "github",
          "action": "create_release"
        }
      }
    ]
  }
}
```

## Pipeline Settings

Release pipeline supports global settings:

```json
{
  "release": {
    "enabled": true,
    "steps": [],
    "settings": {
      "distTarget": "homeboy",
      "dryRun": false
    }
  }
}
```

**Available settings:**
- **`distTarget`** (string): Distribution target identifier
- **`dryRun`** (boolean): Preview pipeline execution without making changes

## Error Handling

Pipeline execution stops on first failure. Failed steps are reported with:
- Step ID and label
- Error message
- Exit code (for extension steps)

Resume from a specific step is not supported. Rerun the entire pipeline after fixing issues.

## Related

- [Release command](../commands/release.md) - Plan and run releases
- [Component schema](../schemas/component-schema.md) - Release configuration structure
- [Extension manifest schema](../schemas/extension-manifest-schema.md) - Extension action definitions
