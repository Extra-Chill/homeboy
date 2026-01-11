---
name: homeboy
description: Use this skill when deploying code to production, executing WP-CLI or PM2 commands on remote servers, querying production databases, managing project/server configurations, performing component-scoped git operations, or when the user mentions Homeboy, deployment, or remote server operations.
version: 0.1.10
---

# Homeboy CLI

CLI for project development and deployment. Provides terminal access to project management, remote CLI operations (WP-CLI, PM2), database queries, deployments, and component-scoped git/build operations.

**CLI documentation**: run `homeboy docs` (and `homeboy docs <topic>`).

## Commands Overview

| Command | Purpose |
|---------|---------|
| `project` | Manage project configurations (create, set, list, show, switch, repair, pin) |
| `component` | Manage standalone component configurations |
| `server` | Manage server configurations (create, show, set, delete, list, key) |
| `git` | Component-scoped git operations (status, commit, push, pull, tag) |
| `version` | Component-scoped version management (show, bump) |
| `build` | Component-scoped builds |
| `wp` | Execute WP-CLI commands on WordPress projects |
| `pm2` | Execute PM2 commands on Node.js projects |
| `db` | Database operations (tables, describe, query, delete-row, drop-table, tunnel) |
| `deploy` | Deploy components to production |
| `ssh` | Execute SSH commands or open interactive shell |
| `module` | Manage, source, install, update, and run Homeboy modules |

## Commands and help

```bash
homeboy project list           # List all projects
homeboy project list --current # Get active project ID
homeboy docs                   # Embedded docs index
homeboy docs <topic...>        # Embedded docs for a topic
homeboy help <command>         # CLI help for any command/subcommand
```

## Safety Guidelines

1. **Deploy**: Always run with `--dry-run` first to preview changes
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

### Remote WordPress Operations
```bash
homeboy wp <project> plugin list
homeboy wp <project> cache flush
homeboy wp <project> core version
```

### Database Queries (Read-Only)
```bash
homeboy db tables <project>
homeboy db describe <project> <table>
homeboy db query <project> "SELECT * FROM wp_options LIMIT 10"
```

### Deployment
```bash
homeboy deploy <project> --dry-run --all    # Preview all deployments
homeboy deploy <project> --outdated         # Deploy changed components only
homeboy deploy <project> <component-id>     # Deploy specific component
```

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

### Version Management (Component-Scoped)
```bash
homeboy version show <component>            # Display current version
homeboy version bump <component> patch      # 0.1.2 → 0.1.3
homeboy version bump <component> minor      # 0.1.2 → 0.2.0
homeboy version bump <component> major      # 0.1.2 → 1.0.0
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
