# Configuration Reference

This page is a field-level reference for Homeboy configuration structures. For
the higher-level system model and core/extension boundary, see
[Architecture Overview](../internals/developer-guide/architecture-overview.md).

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

- `defaults` — Built-in install, version-discovery, deploy, and permission defaults.
- `bench` — Benchmark execution policy.
- `lab` — Preferred runner and runner workspace retention policy.
- `triage` — Triage priority-label configuration.
- `agent_task` — Default backend, secret sources, and provider rotation policy for agent-task dispatch.
- `notifications` — Notification delivery policy for route-less completed operations.
- `worktree_providers` — External worktree lifecycle providers keyed by provider ID. A command provider that sets `commands.list` must also set `list_result_mapping`: JSONPath selectors for `items`, `handle`, `path`, `branch`, `dirty`, `unpushed`, and `primary`. `items` must resolve to one array; each item selector must resolve to exactly one value of its required type (strings for handle/path/branch, booleans for safety values). Homeboy does not infer response envelopes or safety values.
- `github_hosts` — Host-scoped environment for `gh` subprocesses, keyed by GitHub hostname. Component-level `github.hosts` entries override these global defaults.
- `settings` — Generic extension and executor settings, addressed through `/settings/...`.
- `release_gate` — Routing safety policy for release-gate hot commands.
- `artifact_root` — Optional directory where persisted run artifacts are copied. Override per command with `homeboy --artifact-root <dir>` or per process with `HOMEBOY_ARTIFACT_ROOT`.
- `retention` — Bounded cleanup policy shared by terminal-run evidence and runtime resources.
- `update_check` — Enable automatic update check on startup (default: true). Disable with `homeboy config set /update_check false` or set `HOMEBOY_NO_UPDATE_CHECK=1`.
- `resident_services` — Long-running services to restart after `homeboy upgrade` swaps the on-disk binary.

Notification caller context can be supplied per process with
`HOMEBOY_NOTIFICATION_TRANSPORT` and `HOMEBOY_NOTIFICATION_ROUTE`; both are
required together. Explicit `--notification-transport` and
`--notification-route` CLI values take precedence over these environment
variables.

### `BenchConfig`

- `local_execution` — Local benchmark execution policy: `allowed` (default) or `denied`.

### `LabConfig`

- `preferred_runner` — Default Lab runner ID.
- `runner_workspace_ttl` — Optional workspace retention duration for runner materialization.

### `RetentionConfig`

- `terminal_run_days` — Age threshold before terminal persisted-run artifacts are eligible for cleanup (default: 30). Active and unknown run states remain protected.
- `runtime_tmp_days` — Age threshold for Homeboy runtime temporary entries (default: 7).
- `limit` — Maximum persisted-run artifact records inspected per aggregate cleanup invocation (default: 1000).

### `TriageConfig`

- `priority_labels` — Optional labels treated as priority during triage.

### `AgentTaskConfig`

- `default_backend` — Optional default agent-task backend.
- `secrets` — Secret sources keyed by secret name.
- `rotation` — Global provider rotation policy. Per-plan `options.rotation` and per-task `metadata.provider_rotation` take precedence.

### `AgentTaskSecretSource`

- `source` — Secret source kind; defaults to `env`.
- `env_var`
- `path`
- `scope`
- `name`
- `field`
- `value`

### `NotificationConfig`

- `default_transport` — Optional installed extension transport used only when a completed operation has no persisted route.

### `ResidentServiceConfig`

- `id` — Stable service identifier used in upgrade result reporting.
- `systemd_unit` — Unit restarted with `systemctl restart <unit>` when no explicit command is set.
- `restart_command` — Explicit restart command, overriding the systemd default.

### `WorktreeProviderConfig`

- `enabled` — Whether the provider is available; defaults to `true`.
- `kind` — Provider kind. Current supported value: `command`.
- `apply_enabled` — Whether mutating provider operations are enabled.
- `commands` — Provider command argv arrays.
- `list_result_mapping` — Required projection contract when `commands.list` is configured.

### `WorktreeProviderCommands`

- `list`
- `cleanup_preview`
- `cleanup_apply`
- `artifacts_preview`
- `artifacts_apply`

### `WorktreeProviderListResultMapping`

- `items`
- `handle`
- `path`
- `branch`
- `dirty`
- `unpushed`
- `primary`

### `ReleaseGateConfig`

- `local_hot` — Policy for force-local or stale-runner fallback of release-gate hot commands: `fail_closed` (default) or `allowed`. `HOMEBOY_RELEASE_GATE_LOCAL_HOT` overrides this value for one process.

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

- `runtime_type` — Legacy UI/runtime hint (python/shell/cli). CLI ignores this field.
- `run_command` — Shell command to execute when running the extension. Template variables: {{entrypoint}}, {{args}}, {{extensionPath}}, plus project context vars.
- `setup_command` — Shell command to set up the extension (e.g., create venv, install deps).
- `ready_check` — Shell command to check if extension is ready. Exit 0 = ready.
- `env` — Environment variables to set when running the extension.
- `entrypoint` — Entry point file (used in template substitution).
- `args` — Default args template (used in template substitution).
- `default_site` — Default site for this extension (used by some CLI extensions).
- `dependencies` — Legacy UI/runtime hint for Python dependencies to install.
- `playwright_browsers` — Legacy UI/runtime hint for Playwright browsers to install.

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
- `builtin` — Legacy UI action type. CLI parses but does not execute.
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
- `bench`
- `actions`
- `hooks`
- `settings`
- `requires`
- `extra`
- `extension_path`
