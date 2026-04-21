# Project Schema

Project configuration defines deployable environments stored in `projects/<id>.json`.

## Schema

```json
{
  "id": "string",
  "name": "string",
  "domain": "string",
  "server_id": "string",
  "base_path": "string",
  "project_type": "string",
  "component_ids": [],
  "api": {},
  "database": {},
  "local_environment": {},
  "remote_files": {},
  "remote_logs": {},
  "table_prefix": "string",
  "protected_table_patterns": [],
  "unlocked_table_patterns": [],
  "shared_tables": [],
  "table_groupings": [],
  "sub_targets": [],
  "component_groupings": [],
  "cli_path": "string",
  "tools": {},
  "extensions": {}
}
```

## Fields

### Required Fields

- **`id`** (string): Unique project identifier
- **`name`** (string): Human-readable project name
- **`domain`** (string): Project domain name
- **`server_id`** (string): ID of linked server configuration
- **`base_path`** (string): Remote server base path for project files

### Optional Fields

- **`project_type`** (string): Project type identifier (e.g., `"wordpress"`)
- **`component_ids`** (array): List of component IDs linked to this project
- **`api`** (object): API client configuration
  - **`base_url`** (string): API base URL
  - **`enabled`** (boolean): Whether API client is enabled
- **`database`** (object): Database connection settings
  - **`host`** (string): Database host
  - **`port`** (number): Database port (default: 3306)
  - **`name`** (string): Database name
  - **`user`** (string): Database user
  - **`password`** (string): Database password (stored in keychain)
  - **`use_ssh_tunnel`** (boolean): Connect via SSH tunnel
- **`local_environment`** (object): Local development environment
  - **`domain`** (string): Local domain
  - **`site_path`** (string): Local site path
- **`remote_files`** (object): Remote file management
  - **`pinned_files`** (array): List of frequently accessed files
    - **`id`** (string): Unique identifier
    - **`path`** (string): File path relative to base_path
- **`remote_logs`** (object): Remote log management
  - **`pinned_logs`** (array): List of frequently accessed logs
    - **`id`** (string): Unique identifier
    - **`path`** (string): Log path relative to base_path
    - **`tail_lines`** (number): Default line count for tail
- **`table_prefix`** (string): Database table prefix (e.g., `"wp_"`)
- **`protected_table_patterns`** (array): Patterns for protected tables (cannot be deleted)
- **`unlocked_table_patterns`** (array): Patterns for unlocked tables (allow dangerous operations)
- **`shared_tables`** (array): List of shared table names across multi-site installations
- **`table_groupings`** (array): Groupings for organizing tables
  - **`id`** (string): Unique grouping identifier
  - **`name`** (string): Grouping name
  - **`patterns`** (array): Glob patterns matching tables in this group
  - **`member_ids`** (array): Explicit table IDs in this group
  - **`sort_order`** (number): Display sort order
- **`sub_targets`** (array): Sub-target paths for multi-component sites
- **`component_groupings`** (array): Groupings for organizing components
  - **`id`** (string): Unique grouping identifier
  - **`name`** (string): Grouping name
  - **`member_ids`** (array): Component IDs in this group
- **`cli_path`** (string): Project-scoped CLI path used by extension deploy install steps. On any given site the WP-CLI entrypoint is fixed (`wp`, `studio wp`, a Lando wrapper, etc.) and shared by every component deployed there, so this lives at the project layer instead of being repeated per component. Component-level `component_overrides[id].cli_path` still wins as the most-specific escape hatch. If unset, the deploy resolver auto-detects Studio sites (projects whose `base_path` is under `~/Studio/`) and defaults them to `"studio wp"`.
- **`tools`** (object): Project-specific tool configurations
  - Keys are tool identifiers (e.g., `"newsletter"`, `"bandcamp_scraper"`)
  - Values are tool-specific setting objects
- **`extensions`** (object): Extension-specific settings for this project
  - Keys are extension IDs
  - Values are extension setting objects

## Example

```json
{
  "id": "extrachill",
  "name": "Extra Chill",
  "domain": "extrachill.com",
  "server_id": "production",
  "base_path": "/var/www/extrachill",
  "project_type": "wordpress",
  "component_ids": [
    "extrachill-theme",
    "extrachill-api"
  ],
  "api": {
    "base_url": "https://extrachill.com/wp-json",
    "enabled": true
  },
  "database": {
    "host": "localhost",
    "port": 3306,
    "name": "extrachill_db",
    "user": "extrachill_user",
    "use_ssh_tunnel": true
  },
  "local_environment": {
    "domain": "extrachill.local",
    "site_path": "/Users/dev/Sites/extrachill"
  },
  "remote_files": {
    "pinned_files": [
      {
        "id": "wp-config",
        "path": "wp-config.php"
      }
    ]
  },
  "remote_logs": {
    "pinned_logs": [
      {
        "id": "debug",
        "path": "wp-content/debug.log",
        "tail_lines": 100
      }
    ]
  },
  "table_prefix": "wp_",
  "table_groupings": [
    {
      "id": "wordpress-core",
      "name": "WordPress Core",
      "patterns": ["wp_*"],
      "member_ids": [],
      "sort_order": 0
    }
  ],
  "extensions": {
    "wordpress": {
      "settings": {
        "wp_cli_path": "/usr/local/bin/wp"
      }
    }
  }
}
```

## Storage Location

Projects are stored as individual JSON files under the OS config directory:
- **macOS/Linux**: `~/.config/homeboy/projects/<id>.json`
- **Windows**: `%APPDATA%\homeboy\projects\<id>.json`

## Security Notes

Database passwords should not be stored directly in project JSON files. Use the `homeboy auth` command to store credentials securely in the OS keychain. Homeboy automatically retrieves credentials during database operations.

## Related

- [Project command](../commands/project.md) - Manage project configuration
- [Server schema](server-schema.md) - Server linkage configuration
- [Component schema](component-schema.md) - Component linkage configuration
- [API client system](../architecture/api-client.md) - How API authentication works
