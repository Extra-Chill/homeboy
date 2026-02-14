# `homeboy fleet`

Manage fleets — named groups of projects for coordinated operations across multiple sites.

## Synopsis

```sh
homeboy fleet <COMMAND>
```

## Overview

Fleets enable cloud version management by grouping projects that share components. Use fleets to:

- Deploy updates to multiple sites simultaneously
- Check version drift across your network
- Coordinate deployments between staging/production environments
- Keep shared plugins/themes in sync across a WordPress multisite network

**Hierarchy:**
- **Component** → versioned thing (plugin, CLI tool, module)
- **Project** → deployment target (site on a server)
- **Fleet** → named group of projects

## Subcommands

### `create`

```sh
homeboy fleet create <id> [--projects <p1,p2,...>] [--description <text>]
```

Create a new fleet. Projects can be added at creation or later with `fleet add`.

### `show`

```sh
homeboy fleet show <id>
```

Display fleet configuration including project list.

### `set`

```sh
homeboy fleet set <id> --json <JSON>
homeboy fleet set <id> '<JSON>'
```

Update fleet configuration by merging a JSON object.

### `delete`

```sh
homeboy fleet delete <id>
```

Delete a fleet. Does not affect the projects themselves.

### `list`

```sh
homeboy fleet list
```

List all configured fleets.

### `add`

```sh
homeboy fleet add <id> --project <project_id>
homeboy fleet add <id> -p <project_id>
```

Add a project to a fleet. The project must exist.

### `remove`

```sh
homeboy fleet remove <id> --project <project_id>
homeboy fleet remove <id> -p <project_id>
```

Remove a project from a fleet. Does not delete the project.

### `projects`

```sh
homeboy fleet projects <id>
```

List all projects in a fleet with their full configuration.

### `components`

```sh
homeboy fleet components <id>
```

Show component usage across the fleet. Returns a map of component_id → [project_ids].

Useful for understanding which components are shared and where they're deployed.

### `status`

```sh
homeboy fleet status <id>
```

Show component versions for each project in the fleet. Reads local configuration only (no SSH).

Use `fleet check` for drift detection that compares local vs remote versions.

### `check`

```sh
homeboy fleet check <id> [--outdated]
```

Check component drift across the fleet by comparing local and remote versions via SSH.

Uses existing `deploy --check` infrastructure with version_targets pattern matching.

Options:
- `--outdated`: Only show components that need updates (filters out up_to_date)

Returns per-project status with:
- `local_version`: Version from local component files
- `remote_version`: Version fetched from remote server via SSH
- `status`: `up_to_date`, `needs_update`, or `unknown`

Summary includes counts for quick overview.

### `sync`

```sh
homeboy fleet sync <id> [--category <name>] [--dry-run] [--leader <project_id>]
```

Sync OpenClaw agent configurations across all servers in a fleet. Reads a `fleet-sync.json` manifest from `~/.config/homeboy/` that defines what to sync and where from.

Options:
- `--category <name>`: Only sync specific categories (repeatable, comma-separated)
- `--dry-run`: Preview what would be synced without applying changes
- `--leader <project_id>`: Override the leader server (source of truth)

#### Sync Categories

| Category | What it syncs | Method |
|----------|--------------|--------|
| `openclaw-config` | Specific JSON paths in `openclaw.json` (e.g., model fallbacks) | JSON merge |
| `opencode-config` | OpenCode config files and prompts | File copy |
| `opencode-auth` | GitHub Copilot auth tokens | File copy |
| `skills` | Workspace skill directories | Directory tar stream |
| `workspace-files` | Shared workspace files (CODING-STANDARDS.md, etc.) | File copy |

#### Manifest Format (`~/.config/homeboy/fleet-sync.json`)

```json
{
  "leader": "command-server",
  "categories": {
    "openclaw-config": {
      "enabled": true,
      "merge_paths": ["agents.defaults.model.fallbacks"]
    },
    "opencode-config": {
      "enabled": true,
      "files": [".config/opencode/opencode.json", ".config/opencode/prompts/"]
    },
    "opencode-auth": {
      "enabled": true,
      "files": [".local/share/opencode/auth.json"]
    },
    "skills": {
      "enabled": true,
      "items": ["skills/opencode/"]
    },
    "workspace-files": {
      "enabled": false,
      "items": ["CODING-STANDARDS.md", "TOOLS.md"]
    }
  }
}
```

#### Key Behaviors

- **Auto-detects OpenClaw home** on each server (handles `/root/.openclaw/` and `/home/openclaw/.openclaw/`)
- **JSON merge** for `openclaw-config` — merges specific paths without overwriting the full config
- **Skips the leader** — doesn't sync the source of truth to itself
- **Fixes ownership** — automatically `chown`s files for non-root OpenClaw users
- **Category filtering** — sync only what you need with `--category`

#### Example

```sh
# Preview sync across the fleet
homeboy fleet sync fleet-servers --dry-run

# Sync everything
homeboy fleet sync fleet-servers

# Sync only model config
homeboy fleet sync fleet-servers --category openclaw-config

# Sync skills and opencode config
homeboy fleet sync fleet-servers --category skills,opencode-config
```

## Fleet Deployment

Fleets integrate with the deploy command:

```sh
# Deploy component to all projects in a fleet
homeboy deploy my-plugin --fleet production

# Deploy component to ALL projects using it (auto-detected)
homeboy deploy my-plugin --shared
```

See [deploy](deploy.md) for full deployment options.

## Shared Component Detection

To see which components are shared across projects:

```sh
homeboy component shared
# → my-plugin: [site-a, site-b, site-c]
# → homeboy: [project-1, project-2]

homeboy component shared my-plugin
# → [site-a, site-b, site-c]
```

## Example Workflow

```sh
# 1. See what's shared
homeboy component shared

# 2. Create a fleet
homeboy fleet create production --projects site-a,site-b,site-c

# 3. Check for drift
homeboy fleet check production
# → Shows local vs remote versions, identifies outdated components

# 4. Deploy updates
homeboy deploy my-plugin --fleet production
# → Deploys to all projects in fleet

# Or deploy to all users of a component
homeboy deploy my-plugin --shared
# → Auto-detects projects using my-plugin
```

## JSON Output

Top-level fields:

- `command`: action identifier (e.g., `fleet.create`, `fleet.check`)
- `fleet_id`: fleet ID for single-fleet actions
- `fleet`: fleet configuration
- `fleets`: list for `list` command
- `projects`: project details for `projects` command
- `components`: component usage map for `components` command
- `status`: version info per project for `status` command
- `check`: drift detection results for `check` command
- `summary`: aggregate counts for `check` command

Check result fields:
- `project_id`, `server_id`, `status`, `error`
- `components[]`: array with `component_id`, `local_version`, `remote_version`, `status`

Summary fields:
- `total_projects`, `projects_checked`, `projects_failed`
- `components_up_to_date`, `components_needs_update`, `components_unknown`

Sync result fields:
- `leader_project_id`: source of truth server
- `dry_run`: whether this was a preview run
- `projects[]`: per-project results with `project_id`, `server_id`, `status`, `error`
- `projects[].categories[]`: per-category results with `category`, `status`, `error`, `files_synced`
- `summary`: `total_projects`, `projects_synced`, `projects_failed`, `projects_skipped`, `total_categories`, `categories_synced`, `categories_failed`

## Related

- [deploy](deploy.md) — `--fleet` and `--shared` flags
- [component](component.md) — `component shared` command
- [server](server.md) — SSH connection configuration
- [project](project.md) — Project configuration
