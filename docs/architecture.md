# Config Structures

## Configuration

### `ScopedExtensionConfig`

- `version` — Version constraint string (e.g., ">=2.0.0", "^1.0").
- `settings` — Settings passed to the extension at runtime.

### `GitDeployConfig`

- `remote`
- `branch`
- `post_pull` — Commands to run after git pull (e.g., "composer install", "npm run build")
- `tag_pattern` — Pull a specific tag instead of branch HEAD (e.g., "v{{version}}")

### `TestConfig`

- `name`
- `description`
- `tags`
- `extensions`

### `HomeboyConfig`

- `defaults`
- `update_check` — Enable automatic update check on startup (default: true). Disable with `homeboy config set /update_check false` or set HOMEBOY_NO_UPDATE_CHECK=1.

### `InstallMethodsConfig`

- `homebrew`
- `cargo`
- `source`
- `binary`

### `InstallMethodConfig`

- `path_patterns`
- `upgrade_command`
- `list_command`

### `VersionCandidateConfig`

- `file`
- `pattern`

### `DeployConfig`

- `scp_flags`
- `artifact_prefix`
- `default_ssh_port`

### `PermissionsConfig`

- `local`
- `remote`

### `ProvidesConfig`

- `file_extensions` — File extensions this extension can process (e.g., ["php", "inc"]).
- `capabilities` — Capabilities this extension supports (e.g., ["fingerprint", "refactor"]).

### `ScriptsConfig`

- `fingerprint` — Script that extracts structural fingerprints from source files. Receives file content on stdin, outputs FileFingerprint JSON on stdout.
- `refactor` — Script that applies refactoring edits to source files. Receives edit instructions on stdin, outputs transformed content on stdout.

### `RequirementsConfig`

- `extensions`
- `components`

### `DatabaseConfig`

- `cli`

### `DatabaseCliConfig`

- `tables_command`
- `describe_command`
- `query_command`

### `CliHelpConfig`

- `project_id_help`
- `args_help`
- `examples`

### `CliConfig`

- `tool`
- `display_name`
- `command_template`
- `default_cli_path`
- `working_dir_template`
- `settings_flags`
- `help`

### `DiscoveryConfig`

- `find_command`
- `base_path_transform`
- `display_name_command`

### `VersionPatternConfig`

- `extension`
- `pattern`

### `SinceTagConfig`

- `extensions` — File extensions to scan (e.g., [".php"]).
- `placeholder_pattern` — Regex pattern matching placeholder versions in `@since` tags. Default: `0\.0\.0|NEXT|TBD|TODO|UNRELEASED|x\.x\.x`

### `BuildConfig`

- `artifact_extensions`
- `script_names`
- `command_template`
- `extension_script`
- `pre_build_script`
- `artifact_pattern` — Default artifact path pattern with template support. Supports: {component_id}, {local_path}
- `cleanup_paths` — Paths to clean up after successful deploy (e.g., node_modules, vendor, target)

### `LintConfig`

- `extension_script`

### `RuntimeConfig`

- `runtime_type` — Desktop app runtime type (python/shell/cli). CLI ignores this field.
- `run_command` — Shell command to execute when running the extension. Template variables: {{entrypoint}}, {{args}}, {{extensionPath}}, plus project context vars.
- `setup_command` — Shell command to set up the extension (e.g., create venv, install deps).
- `ready_check` — Shell command to check if extension is ready. Exit 0 = ready.
- `env` — Environment variables to set when running the extension.
- `entrypoint` — Entry point file (used in template substitution).
- `args` — Default args template (used in template substitution).
- `default_site` — Default site for this extension (used by some CLI extensions).
- `dependencies` — Desktop app: Python dependencies to install.
- `playwright_browsers` — Desktop app: Playwright browsers to install.

### `InputConfig`

- `id`
- `input_type`
- `label`
- `placeholder`
- `default`
- `min`
- `max`
- `options`
- `arg`

### `OutputConfig`

- `schema`
- `display`
- `selectable`

### `ActionConfig`

- `id`
- `label`
- `action_type`
- `endpoint`
- `method`
- `requires_auth`
- `payload`
- `command`
- `builtin` — Builtin action type (Desktop app only). CLI parses but does not execute.
- `column` — Column identifier for copy-column builtin action.

### `SettingConfig`

- `id`
- `setting_type`
- `label`
- `placeholder`
- `default`

### `RemoteFileConfig`

- `pinned_files`

### `RemoteLogConfig`

- `pinned_logs`

### `ApiConfig`

- `enabled`
- `base_url`
- `auth`

### `AuthConfig`

- `header`
- `variables`
- `login`
- `refresh`

### `AuthFlowConfig`

- `endpoint`
- `method`
- `body`
- `store`

### `ToolsConfig`

- `bandcamp_scraper`
- `newsletter`

### `BandcampScraperConfig`

- `default_tag`

### `NewsletterConfig`

- `sendy_list_id`

## Manifests

### `ExtensionManifest`

- `id`
- `name`
- `version`
- `provides`
- `scripts`
- `icon`
- `description`
- `author`
- `homepage`
- `source_url`
- `deploy`
- `audit`
- `executable`
- `platform`
- `cli`
- `build`
- `lint`
- `test`
- `actions`
- `hooks`
- `settings`
- `requires`
- `extra`
- `extension_path`
