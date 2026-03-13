# Config Directory Structure

> **Important:** Homeboy uses centralized configuration only. There is no repo-local config file (no .homeboy.toml or .homeboy directory). All configuration lives in `~/.config/homeboy/`.

Homeboy stores all configuration in a universal directory location across operating systems.

## Location

Homeboy configuration lives under:

### macOS

```
~/.config/homeboy/
```

### Linux

```
~/.config/homeboy/
```

### Windows

```
%APPDATA%\homeboy\
```

Typically: `C:\Users\<username>\AppData\Roaming\homeboy\`

## Directory Structure

```
~/.config/homeboy/
├── homeboy/
│   └── homeboy.json           # Global app configuration
├── projects/
│   ├── <project_id>.json       # Project configurations
│   └── ...
├── servers/
│   ├── <server_id>.json       # Server configurations
│   └── ...
├── components/
│   ├── <component_id>.json    # Component configurations
│   └── ...
├── fleets/
│   ├── <fleet_id>.json        # Fleet configurations
│   └── ...
├── extensions/
│   ├── <extension_id>/
│   │   ├── <extension_id>.json   # Extension manifest
│   │   ├── docs/             # Extension documentation
│   │   └── ...              # Extension files
│   └── ...
├── keys/                     # SSH private keys (optional)
│   ├── <key_name>
│   └── ...
└── backups/                  # Configuration backups (optional)
    └── ...
```

## File Details

### Global App Configuration

**File:** `~/.config/homeboy/homeboy.json`

Contains global Homeboy settings. Created automatically on first run with defaults.

```json
{
  "storage": "builtin-filesystem",
  "installedModules": []
}
```

### Project Configurations

**Directory:** `~/.config/homeboy/projects/`

Each project is a separate JSON file named after the project ID.

**Example:** `~/.config/homeboy/projects/extrachill.json`

```json
{
  "id": "extrachill",
  "name": "Extra Chill",
  "domain": "extrachill.com",
  "server_id": "production",
  "component_ids": ["theme", "api"]
}
```

### Server Configurations

**Directory:** `~/.config/homeboy/servers/`

Each server is a separate JSON file named after the server ID.

**Example:** `~/.config/homeboy/servers/production.json`

```json
{
  "id": "production",
  "name": "Production Server",
  "host": "example.com",
  "user": "deploy"
}
```

### Component Configurations

**Directory:** `~/.config/homeboy/components/`

Each component is a separate JSON file named after the component ID.

**Example:** `~/.config/homeboy/components/theme.json`

```json
{
  "id": "theme",
  "local_path": "/home/dev/theme",
  "remote_path": "wp-content/themes/theme"
}
```

### Fleet Configurations

**Directory:** `~/.config/homeboy/fleets/`

Each fleet is a separate JSON file named after the fleet ID. Fleets group projects for coordinated operations.

**Example:** `~/.config/homeboy/fleets/production.json`

```json
{
  "id": "production",
  "project_ids": ["site-a", "site-b", "site-c"],
  "description": "Production sites sharing common plugins"
}
```

### Extension Directory

**Directory:** `~/.config/homeboy/extensions/`

Each extension is a subdirectory containing:
- Extension manifest: `<extension_id>/<extension_id>.json`
- Extension documentation: `<extension_id>/docs/`
- Extension files: `<extension_id>/` (executables, scripts, etc.)

**Example:** `~/.config/homeboy/extensions/wordpress/wordpress.json`

Extensions are installed via:
- Git clone (remote extensions)
- Symlink (local development extensions)

### Keys Directory

**Directory:** `~/.config/homeboy/keys/`

Stores SSH private keys managed by Homeboy (optional). Keys can be referenced via relative paths in server configurations.

**Example:** `keys/production_key`

### Backups Directory

**Directory:** `~/.config/homeboy/backups/`

Configuration backups created by Homeboy (optional). Created before destructive operations.

## File Operations

Homeboy does not write to directories outside the config directory:
- **No repo-local config files**: Configuration is centralized
- **No .homeboy directories**: Avoids repo contamination
- **Cross-repo compatibility**: Multiple repos can reference the same configurations

## Auto-creation

Directories are created automatically when needed:
- homeboy/ — First run
- projects/ — First project created
- servers/ — First server created
- components/ — First component created
- extensions/&lt;extension_id&gt;/ — Extension installed
- keys/ — Key referenced in server config
- backups/ — Backup created

## Manual Configuration Editing

While Homeboy provides CLI commands for most operations, configurations can be edited manually:

### Editing Tips

1. **Use JSON validators**: Ensure valid JSON syntax
2. **Backup first**: Copy file before editing
3. **Reload changes**: Some changes require command restart
4. **Reference schemas**: See schema documentation for field definitions

### Schema References

- [Component schema](../schemas/component-schema.md)
- [Project schema](../schemas/project-schema.md)
- [Server schema](../schemas/server-schema.md)
- [Fleet schema](../schemas/fleet-schema.md)
- [Extension manifest schema](../schemas/extension-manifest-schema.md)

## Migration and Backups

### Backup Strategy

Homeboy creates backups before:
- Deleting configurations
- Major schema updates (optional)
- Bulk import operations

Backups are stored in `~/.config/homeboy/backups/` with timestamps.

### Export Configurations

Export all configurations to archive:

```bash
tar czf homeboy-config-backup.tar.gz ~/.config/homeboy/
```

### Import Configurations

Restore from backup:

```bash
tar xzf homeboy-config-backup.tar.gz -C ~/.config/
```

## Security Permissions

### Directory Permissions

Config directories should be restricted to user only:

```bash
chmod 700 ~/.config/homeboy
chmod 700 ~/.config/homeboy/keys
```

### File Permissions

Configuration files should be readable only by user:

```bash
chmod 600 ~/.config/homeboy/projects/*.json
chmod 600 ~/.config/homeboy/servers/*.json
chmod 600 ~/.config/homeboy/components/*.json
```

### SSH Keys

SSH private keys must be restricted:

```bash
chmod 600 ~/.config/homeboy/keys/*
```

## Troubleshooting

### Permission Denied Errors

If Homeboy reports permission errors:

```bash
# Fix permissions
chmod 700 ~/.config/homeboy
chmod 600 ~/.config/homeboy/projects/*.json
chmod 600 ~/.config/homeboy/servers/*.json
chmod 600 ~/.config/homeboy/components/*.json
```

### Directory Not Found

If Homeboy cannot find config directory:

1. Verify config directory location for your platform
2. Create directory manually: `mkdir -p ~/.config/homeboy`
3. Run `homeboy status --full` to inspect current Homeboy context

### Corrupt Configuration

If configuration file is invalid:

1. Restore from backup in `~/.config/homeboy/backups/`
2. Or delete corrupt file and recreate via CLI commands

## Related

- [Init command](../commands/init.md) - Deprecated alias for the full status report
- [Config command](../commands/config.md) - Manage global configuration
- [Project command](../commands/project.md) - Manage project configurations
- [Server command](../commands/server.md) - Manage server configurations
- [Component command](../commands/component.md) - Manage component configurations
- [Fleet command](../commands/fleet.md) - Manage fleet configurations
- [Extension command](../commands/extension.md) - Manage extension installations
