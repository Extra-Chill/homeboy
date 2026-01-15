---
name: homeboy
description: Use this skill when deploying code to production, executing module-provided CLI tools or remote process managers over SSH, querying production databases, managing project/server configurations, performing component-scoped git operations, or when the user mentions Homeboy, deployment, or remote server operations.
version: 0.2.3
---

# Homeboy CLI

CLI for project development and deployment. Provides terminal access to project management, remote CLI tool passthrough (via modules), database queries, deployments, and component-scoped git/build operations.

**CLI documentation**: run `homeboy docs` (and `homeboy docs <topic>`).

## Commands Overview

| Command | Purpose |
|---------|---------|
| `project` | Manage project configurations (create, set, list, show, repair, components, pin) |
| `component` | Manage standalone component configurations |
| `server` | Manage server configurations (create, show, set, delete, list, key) |
| `changes` | View uncommitted/unreleased changes for components or projects |
| `git` | Component-scoped git operations (status, commit, push, pull, tag) |
| `version` | Component-scoped version management (show, bump, set) |
| `changelog` | Changelog operations (add entries, bulk add) |
| `build` | Component-scoped builds |
| `<module cmd>` | Module-provided CLI tool passthrough (for example: `wp`, `pm2`) |
| `db` | Database operations (tables, describe, query, delete-row, drop-table, tunnel) |
| `deploy` | Deploy components to production |
| `ssh` | Execute SSH commands or open interactive shell |
| `module` | Manage, source, install, update, and run Homeboy modules |

## Commands and help

```bash
homeboy project list           # List all projects
homeboy docs                   # Embedded docs index
homeboy docs <topic...>        # Embedded docs for a topic
homeboy help <command>         # CLI help for any command/subcommand
```

## Safety Guidelines

1. **Deploy**: Confirm target, scope, and impact first
2. **Database**: Most `db` operations are read-only (tables, describe, query). Write operations exist (delete-row, drop-table) but require explicit confirmation.
3. **SSH**: Exercise caution with destructive commands on production servers
4. **PM2**: `restart` affects live services - confirm intent before executing

## Common patterns

### Local Development Pipeline
```bash
homeboy version bump <component> patch   # Bump version
homeboy git commit <component> "msg"     # Stage all and commit
homeboy git push <component>             # Push to remote
homeboy build <component>                # Run build command
```

### Remote CLI Tool Operations (Module-Provided)
List available tools:

```bash
homeboy module list
```

Run a module-provided tool via the module system:

```bash
homeboy module run <moduleId> --project <projectId>
```

### Database Queries (Read-Only)
```bash
homeboy db tables <project>
homeboy db describe <project> <table>
homeboy db query <project> "SELECT * FROM some_table LIMIT 10"
```

### Deployment
```bash
homeboy deploy <project> --all             # Deploy all components
homeboy deploy <project> --outdated         # Deploy changed components only
homeboy deploy <project> <component-id>     # Deploy specific component
```

### Changes (View Uncommitted/Unreleased)
View changes since last tag or version commit:
```bash
homeboy changes <component>                       # Show changes for single component
homeboy changes <project> <component_ids...>      # Show changes for specific components in project
homeboy changes --project <project>               # Show changes for all project components
homeboy changes --project <project> <components>  # Alternative filter syntax
homeboy changes --cwd                             # Show changes for current directory
homeboy changes <component> --git-diffs           # Include full diff output
```

Output includes:
- `latestTag` - Most recent git tag (or null)
- `baselineSource` - How baseline was determined: `tag`, `version_commit`, or `last_n_commits`
- `commits` - Commits since baseline
- `uncommitted` - Staged, unstaged, and untracked files
- `warning` - Alert if no tags found (showing fallback baseline)

### Git Operations (Component-Scoped)
No more `cd` to directories - operate on components by name:
```bash
homeboy git status <component>              # Show git status
homeboy git commit <component> "message"    # Stage all and commit
homeboy git push <component>                # Push to remote
homeboy git push <component> --tags         # Push with tags
homeboy git pull <component>                # Pull from remote
homeboy git tag <component> v1.0.0          # Create lightweight tag
homeboy git tag <component> v1.0.0 -m "msg" # Create annotated tag
```

### Bulk Git Operations (Multi-Component)
Use `--json` flag to operate on multiple components at once:
```bash
# Bulk commit with per-component messages
homeboy git commit --json '{"components":[{"id":"comp1","message":"msg1"},{"id":"comp2","message":"msg2"}]}'

# Bulk status check
homeboy git status --json '{"componentIds":["comp1","comp2","comp3"]}'

# Bulk push with tags
homeboy git push --json '{"componentIds":["comp1","comp2"],"tags":true}'

# Bulk pull
homeboy git pull --json '{"componentIds":["comp1","comp2"]}'
```

JSON input supports:
- Inline JSON string
- `-` for stdin
- `@file.json` to read from file

### Changelog Operations
```bash
homeboy changelog add <component> "message"  # Add single entry
homeboy changelog add --cwd "message"        # Add to cwd changelog

# Bulk add with JSON
homeboy changelog add --json '{"componentId":"comp1","messages":["Change 1","Change 2"]}'
```

### Version Management (Component-Scoped)
```bash
homeboy version show <component>            # Display current version
homeboy version bump <component> patch      # 0.1.2 → 0.1.3
homeboy version bump <component> minor      # 0.1.2 → 0.2.0
homeboy version bump <component> major      # 0.1.2 → 1.0.0
homeboy version set <component> 1.2.3       # Set version directly
```

### Build (Component-Scoped)
```bash
homeboy build <component>                   # Run component's build_command
```

### Release Workflow
```bash
homeboy git commit <component> "Release v1.0.0"
homeboy git tag <component> v1.0.0
homeboy git push <component> --tags         # Triggers CI/CD
```

### Subtargets
Some commands accept an optional first trailing argument that is treated as a *subtarget* when the project is configured with `sub_targets`.

- `wp`: the first arg may be a subtarget identifier.
- `db`: most subcommands accept a trailing list where the first value may be a subtarget identifier.

Refer to the full CLI documentation for complete command reference and configuration details.
