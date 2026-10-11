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
      "remote_path": "string",
      "deployment_provider": { "extension": "string", "provider": "string", "contract": "string" },
      "deployment_provider_input": {}
    }
  ],
  "component_overrides": {},
  "services": [],
  "changelog_next_section_label": "string",
  "changelog_next_section_aliases": [],
  "cli_path": "string",
  "extensions": {},
  "hooks": {
    "post:deploy": ["wp cache flush --path={{base_path}} --allow-root"],
    "post:deploy:project": ["wp cache purge --all --path={{base_path}} --allow-root"]
  },
  "deployment_provenance": {
    "mode": "accepted-ref",
    "forge_evidence": [{
      "sha": "0123456789abcdef0123456789abcdef01234567",
      "forge": "github",
      "reference": "https://github.com/acme/component/pull/42"
    }]
  }
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
- **`remote_files`** (object): Remote file management
  - **`pinned_files`** (array): List of frequently accessed files
    - **`path`** (string): File path relative to base_path
    - **`label`** (string): Optional display label
- **`table_prefix`** (string): Database table prefix (e.g., `"wp_"`)
- **`shared_tables`** (array): List of shared table names across multi-site installations
- **`sub_targets`** (array): Sub-target paths for multi-component sites
- **`components`** (array): Project-attached component checkouts. Each entry requires `id` and `local_path`; optional `remote_path` overrides the repo-owned component `remote_path` for this project so the same component can deploy to projects with different filesystem layouts. `deployment_provider_input` is opaque project-only target input for a provider that declares layered input support. A repository provider policy becomes the selected route only when this attachment supplies `deployment_provider_input` or the legacy `deployment_provider` override; otherwise deployment uses the standard route, so a component that declares an optional provider stays deployable as a server artifact from projects that do not configure that provider's target. `homeboy deploy --target server|provider` overrides the inferred route for one invocation. `deployment_provider` cannot be combined with `deployment_provider_input`.
- **`component_overrides`** (object): Per-component project overrides keyed by component ID. These remain the most-specific deploy overrides and take precedence over `components[].remote_path`.
- **`services`** (array): Service names checked by project health status
- **`changelog_next_section_label`** (string): Project-level changelog next-section label override
- **`changelog_next_section_aliases`** (array): Additional labels accepted for the next changelog section
- **`cli_path`** (string): Project-scoped CLI path used by extension deploy install steps. On any given site the WP-CLI entrypoint is fixed (`wp`, a Lando wrapper, a project-specific tool, etc.) and shared by every component deployed there, so this lives at the project layer instead of being repeated per component. Component-level `component_overrides[id].cli_path` still wins as the most-specific escape hatch. If unset, the deploy resolver falls back to the extension default CLI path and then the extension tool name, usually `wp`.
- **`extensions`** (object): Extension-specific settings for this project
  - Keys are extension IDs
  - Values are flat extension setting objects; `version` is reserved for extension version constraints
- **`hooks`** (object): Lifecycle hooks declared on the project itself (the deploy target), distinct from `component_overrides[id].hooks`. For a component-scoped event (currently `post:deploy`), these merge into every component deployed to this project, between extension hooks and component hooks. `post:deploy:project` is a separate, project-scoped event that runs once per deploy invocation, after the last component — not once per component — and is declared only here; extensions and components cannot contribute to it. See [hooks](../../architecture/hooks.md).
- **`deployment_provenance`** (object): Optional fail-closed deployment source policy. Omit it to preserve legacy deploy behavior.
  - **`mode`**: `any` (default), `immutable-ref`, `release`, or `accepted-ref`.
  - **`forge_evidence`**: Optional SHA-bound forge records (`sha`, `forge`, `reference`). `accepted-ref` accepts a validated release set, a resolved release tag, or an exact SHA matching one of these records.
  - Policies other than `any` reject `--head` and unverifiable refs before source checkout, dependency installation, build, or remote deployment. `--force` cannot bypass them. The resolved policy evidence is included in every deploy result.

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
  "remote_files": {
    "pinned_files": [
      {
        "path": "wp-config.php",
        "label": "Config"
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

## Related

- [Project command](../../commands/project.md) - Manage project configuration
- [Server schema](server-schema.md) - Server linkage configuration
- [Component schema](component-schema.md) - Component linkage configuration
