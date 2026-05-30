# `homeboy fleet`

Manage fleets — named groups of projects for coordinated operations across multiple sites.

## Synopsis

```sh
homeboy fleet <COMMAND>
```

## Overview

Fleets group projects that share components or operational policies. Use fleets
to:

- Inspect status and version drift across a project group
- Coordinate deployments between staging and production environments
- Keep shared components in sync across multiple sites, apps, or servers
- Produce read-only attention reports for a group of environments

**Hierarchy:**
- **Component** → versioned thing (plugin, CLI tool, extension)
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
homeboy fleet set <id> --base64 <BASE64_JSON>
```

Update fleet configuration by merging a JSON object.
Arbitrary fleet updates must use `--json` or `--base64`; positional JSON and positional `key=value` updates are not accepted.

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
homeboy fleet status <id> --cached
homeboy fleet status <id> --health-only
```

Show live component versions and server health for each project in the fleet via SSH.

- `--cached`: use locally cached versions instead of a live SSH check.
- `--health-only`: skip component versions and report only server health.
- Use `fleet check` for drift detection that compares local vs remote versions.

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

### `exec`

```sh
homeboy fleet exec <id> -- <command>
```

Runs a command across projects in the fleet via each project's configured SSH connection. `fleet exec` participates in resource-policy warnings because it can create heavy remote work, but it intentionally stays local for Lab offload: the command depends on local fleet, project, and server configuration before opening those SSH sessions, and runner-side config parity is not guaranteed.

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
- `status`: live or cached version and health info per project for `status` command
- `check`: drift detection results for `check` command
- `summary`: aggregate counts for `check` command

Check result fields:
- `project_id`, `server_id`, `status`, `error`
- `components[]`: array with `component_id`, `local_version`, `remote_version`, `status`

Summary fields:
- `total_projects`, `projects_checked`, `projects_failed`
- `components_up_to_date`, `components_needs_update`, `components_unknown`

## Related

- [deploy](deploy.md) — `--fleet` and `--shared` flags
- [component](component.md) — `component shared` command
- [server](server.md) — SSH connection configuration
- [project](project.md) — Project configuration
