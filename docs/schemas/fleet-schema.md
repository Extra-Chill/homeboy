# Fleet Schema

Fleet configuration defines named groups of projects stored in `fleets/<id>.json`.

## Schema

```json
{
  "id": "string",
  "project_ids": ["string"],
  "description": "string"
}
```

## Fields

### Required Fields

- **`id`** (string): Unique fleet identifier
- **`project_ids`** (array of strings): List of project IDs in this fleet

### Optional Fields

- **`description`** (string): Human-readable description of the fleet's purpose

## Example

```json
{
  "id": "production",
  "project_ids": [
    "extrachill",
    "sarai-chinwag",
    "chubes-net"
  ],
  "description": "Production WordPress sites sharing common plugins"
}
```

## Use Cases

### Multi-Site Deployment

Group sites that share components for coordinated deployments:

```bash
# Deploy a shared plugin to all production sites
homeboy deploy my-plugin --fleet production
```

### Environment Grouping

Separate staging and production environments:

```json
{
  "id": "staging",
  "project_ids": ["site-a-staging", "site-b-staging"],
  "description": "Staging environments for testing"
}
```

### Component-Based Grouping

Group by shared technology:

```json
{
  "id": "wordpress-sites",
  "project_ids": ["blog", "shop", "docs"],
  "description": "All WordPress-based sites"
}
```

## Storage Location

Fleets are stored as individual JSON files under the OS config directory:
- **macOS/Linux**: `~/.config/homeboy/fleets/<id>.json`
- **Windows**: `%APPDATA%\homeboy\fleets\<id>.json`

## Fleet Operations

### Create a Fleet

```bash
homeboy fleet create production --projects site-a,site-b,site-c
```

### Add/Remove Projects

```bash
homeboy fleet add production --project new-site
homeboy fleet remove production --project old-site
```

### Check Fleet Status

```bash
# Live version and health info via SSH
homeboy fleet status production
homeboy fleet status production --health-only

# Cached version info
homeboy fleet status production --cached

# Drift detection (compares local vs remote via SSH)
homeboy fleet check production
homeboy fleet check production --outdated
```

### Deploy to Fleet

```bash
# Deploy specific component to fleet
homeboy deploy my-plugin --fleet production

# Deploy to all projects using a component
homeboy deploy my-plugin --shared
```

## Shared Component Detection

Before creating fleets, see which components are shared:

```bash
homeboy component shared
# → my-plugin: [site-a, site-b, site-c]
# → homeboy: [project-1, project-2]
```

This helps identify natural groupings for fleet creation.

## Related

- [Fleet command](../commands/fleet.md) - Fleet management commands
- [Deploy command](../commands/deploy.md) - `--fleet` and `--shared` flags
- [Component command](../commands/component.md) - `component shared` command
- [Project schema](project-schema.md) - Project configuration
