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
  "aliases": [],
  "domain": "string",
  "server_id": "string",
  "base_path": "string",
  "path_roots": {},
  "api": {},
  "database": {},
  "remote_files": {},
  "remote_logs": {},
  "table_prefix": "string",
  "shared_tables": [],
  "sub_targets": [],
  "components": [
    {
      "id": "string",
      "local_path": "string",
      "remote_path": "string"
    }
  ],
  "component_overrides": {},
  "services": [],
  "changelog_next_section_label": "string",
  "changelog_next_section_aliases": [],
  "cli_path": "string",
  "extensions": {}
}
```

## Fields

### Required Fields

- **`id`** (string): Unique project identifier. The ID is the filename key and is not serialized inside the stored project object.

### Optional Fields

- **`aliases`** (array): Alternate IDs accepted for project lookup
- **`domain`** (string): Project domain name
- **`server_id`** (string): ID of linked server configuration
- **`base_path`** (string): Local or remote base path for project files
- **`path_roots`** (object): Named path roots used by deploy/path resolution
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
- **`shared_tables`** (array): List of shared table names across multi-site installations
- **`sub_targets`** (array): Sub-target paths for multi-component sites
- **`components`** (array): Project-attached component checkouts. Each entry requires `id` and `local_path`; optional `remote_path` overrides the repo-owned component `remote_path` for this project so the same component can deploy to projects with different filesystem layouts.
- **`component_overrides`** (object): Per-component project overrides keyed by component ID. These remain the most-specific deploy overrides and take precedence over `components[].remote_path`.
- **`services`** (array): Service names checked by project/fleet health status
- **`changelog_next_section_label`** (string): Project-level changelog next-section label override
- **`changelog_next_section_aliases`** (array): Additional labels accepted for the next changelog section
- **`cli_path`** (string): Project-scoped CLI path used by extension deploy install steps. On any given site the WP-CLI entrypoint is fixed (`wp`, a Lando wrapper, a project-specific tool, etc.) and shared by every component deployed there, so this lives at the project layer instead of being repeated per component. Component-level `component_overrides[id].cli_path` still wins as the most-specific escape hatch. If unset, the deploy resolver falls back to the extension default CLI path and then the extension tool name, usually `wp`.
- **`extensions`** (object): Extension-specific settings for this project
  - Keys are extension IDs
  - Values are flat extension setting objects; `version` is reserved for extension version constraints

## Example

```json
{
  "id": "extrachill",
  "domain": "extrachill.com",
  "server_id": "production",
  "base_path": "/var/www/extrachill",
  "components": [
    {
      "id": "extrachill-theme",
      "local_path": "/Users/dev/Developer/extrachill-theme",
      "remote_path": "wp-content/themes/extrachill-theme"
    },
    {
      "id": "extrachill-api",
      "local_path": "/Users/dev/Developer/extrachill-api",
      "remote_path": "wp-content/plugins/extrachill-api"
    }
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
  "services": ["nginx", "php8.4-fpm"],
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

- [Project command](../../commands/project.md) - Manage project configuration
- [Server schema](server-schema.md) - Server linkage configuration
- [Component schema](component-schema.md) - Component linkage configuration
- [API client system](../../architecture/api-client.md) - How API authentication works
