# Project Schema

Project configuration defines environment-specific context stored in
`projects/<id>.json`: server bindings, domains, local paths, remote paths,
project-scoped CLI settings, API settings, database settings, and component
attachments. Components can run local quality loops without a project; projects
matter when the workflow needs environment context.

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
  "components": [
    {
      "id": "string",
      "local_path": "string",
      "remote_path": "string"
    }
  ],
  "component_groupings": [],
  "component_overrides": {},
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
  - **`proxy_url`** (string): Optional HTTP/SOCKS proxy URL for API requests, e.g. `socks5://127.0.0.1:8080`
  - **`auth`** (object): Optional API auth configuration with a header template and variables sourced from `keychain`, `env`, or `config`
  - API POST/PUT/PATCH calls can send form data with `homeboy api <project> post <endpoint> --form key=value`.
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
- **`components`** (array): Project-attached component checkouts. Each entry requires `id` and `local_path`; optional `remote_path` overrides the repo-owned component `remote_path` for this project so the same component can deploy to projects with different filesystem layouts.
- **`component_groupings`** (array): Groupings for organizing components
  - **`id`** (string): Unique grouping identifier
  - **`name`** (string): Grouping name
  - **`member_ids`** (array): Component IDs in this group
- **`component_overrides`** (object): Per-component project overrides keyed by component ID. These remain the most-specific deploy overrides and take precedence over `components[].remote_path`.
- **`cli_path`** (string): Project-scoped CLI path used by extension deploy install steps. On any given site the WP-CLI entrypoint is fixed (`wp`, `studio wp`, a Lando wrapper, etc.) and shared by every component deployed there, so this lives at the project layer instead of being repeated per component. Component-level `component_overrides[id].cli_path` still wins as the most-specific escape hatch. If unset, the deploy resolver auto-detects Studio sites (projects whose `base_path` is under `~/Studio/`) and defaults them to `"studio wp"`.
- **`tools`** (object): Project-specific tool configurations
  - Keys are tool identifiers (e.g., `"newsletter"`, `"bandcamp_scraper"`)
  - Values are tool-specific setting objects
- **`extensions`** (object): Extension-specific settings for this project
  - Keys are extension IDs
  - Values are flat extension setting objects; `version` is reserved for extension version constraints

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
    "enabled": true,
    "proxy_url": "socks5://127.0.0.1:8080",
    "auth": {
      "header": "Authorization: Bearer {{token}}",
      "variables": {
        "token": {
          "source": "keychain"
        }
      }
    }
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
      "wp_cli_path": "/usr/local/bin/wp"
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
