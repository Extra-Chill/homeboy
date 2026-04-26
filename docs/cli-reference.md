# CLI Commands

## Commands

- `homeboy` — 

## Command Arguments

### `ApiArgs`

- `project_id` — Project ID
- `command`

### `AuditArgs`

- `component_id` — Component ID or direct filesystem path to audit
- `conventions` — Only show discovered conventions (skip findings)
- `baseline` — Save current audit state as baseline for future comparisons
- `ignore_baseline` — Skip baseline comparison even if a baseline exists

### `AuthArgs`

- `command`

### `BuildArgs`

- `json` — JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]}
- `target_id` — Target ID: component ID or project ID (when using --all)
- `component_ids` — Additional component IDs (enables project/component order detection)
- `all` — Build all components in the project
- `path` — Override local_path for this build (use a workspace clone or temp checkout)

### `ChangelogArgs`

- `show_self` — Show Homeboy's own changelog (release notes)
- `command`

### `ChangesArgs`

- `target_id` — Target ID: component ID (single mode) or project ID (if followed by component IDs)
- `component_ids` — Component IDs to filter (when target_id is a project)
- `project` — Show changes for all components in a project (alternative to positional project mode)
- `json` — JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]}
- `since` — Compare against specific tag instead of latest
- `git_diffs` — Include commit range diff in output (uncommitted diff is always included)

### `CleanupArgs`

- `component_id` — Component to check (omit for all components)
- `severity` — Show only issues of a specific severity: error, warning, info
- `category` — Show only issues in a specific category: local_path, remote_path, version_targets, extensions

### `CliArgs`

- `tool`
- `identifier`
- `args`

### `ComponentArgs`

- `command`

### `ConfigArgs`

- `command`

### `DbArgs`

- `command`

### `DeployArgs`

- `target_id` — Target ID: project ID or component ID (order is auto-detected)
- `component_ids` — Additional component IDs (enables project/component order detection)
- `project` — Explicit project ID (takes precedence over positional detection)
- `component` — Explicit component IDs (takes precedence over positional)
- `json` — JSON input spec for bulk operations
- `all` — Deploy all configured components
- `outdated` — Deploy only outdated components
- `dry_run` — Preview what would be deployed without executing
- `check` — Check component status without building or deploying
- `force` — Deploy even with uncommitted changes
- `projects` — Deploy to multiple projects (comma-separated or repeated)
- `fleet` — Deploy to all projects in a fleet
- `shared` — Deploy to all projects using the specified component(s)
- `keep_deps` — Keep build dependencies (skip post-deploy cleanup)

### `DocsArgs`

- `command`
- `topic` — Topic path (e.g., 'commands/deploy') or 'list' to show available topics

### `ExtensionArgs`

- `command`

### `FileArgs`

- `command`

### `FleetArgs`

- `command`

### `GitArgs`

- `command`

### `InitArgs`

- `all` — Show all components, extensions, projects, and servers
- `json` — Accept --json for compatibility (output is JSON by default)

### `LintArgs`

- `component` — Component name to lint
- `summary` — Show compact summary instead of full output
- `file` — Lint only a single file (path relative to component root)
- `glob` — Lint only files matching glob pattern (e.g., "inc/**/*.php")
- `changed_only` — Lint only files modified in the working tree (staged, unstaged, untracked)
- `errors_only` — Show only errors, suppress warnings
- `sniffs` — Only check specific sniffs (comma-separated codes)
- `exclude_sniffs` — Exclude sniffs from checking (comma-separated codes)
- `category` — Filter by category: security, i18n, yoda, whitespace
- `setting` — Override settings as key=value pairs
- `path` — Override local_path for this lint run (use a workspace clone or temp checkout)
- `json` — Accept --json for compatibility (output is JSON by default)

### `LogsArgs`

- `command`

### `DynamicSetArgs`

- `id` — Entity ID (optional if provided in JSON body)
- `spec` — JSON spec (positional, supports @file and - for stdin)
- `json` — Explicit JSON spec (takes precedence over positional)
- `base64` — Base64-encoded JSON spec (bypasses shell escaping issues)
- `replace` — Replace these fields instead of merging arrays
- `extra` — Dynamic key=value flags (e.g., --remote_path /var/www). When combined with --json, add '--' separator first: `homeboy component set ID --json '{}' -- --key value`

### `ProjectArgs`

- `command`

### `RefactorArgs`

- `command`

### `ReleaseArgs`

- `component_id` — Component ID
- `bump_type`
- `dry_run` — Preview what will happen without making changes
- `json` — Accept --json for compatibility (output is JSON by default)
- `deploy` — Deploy to all projects using this component after release
- `recover` — Recover from an interrupted release (tag + push current version)

### `ServerArgs`

- `command`

### `KeyArgs`

- `command`

### `SshArgs`

- `target` — Target ID (project or server; project wins when ambiguous)
- `command` — Command to execute (omit for interactive shell).  Examples: homeboy ssh my-project -- ls -la homeboy ssh my-project -- wp plugin list  If you need shell operators (&&, |, redirects), pass a single quoted string: homeboy ssh my-project "cd /var/www && ls | head"
- `as_server` — Force interpretation as server ID
- `subcommand`

### `StatusArgs`

- `uncommitted` — Show only components with uncommitted changes
- `needs_bump` — Show only components that need a version bump
- `ready` — Show only components ready to deploy
- `docs_only` — Show only components with docs-only changes
- `all` — Show all components regardless of current directory context

### `StackArgs`

- `command`

### `TestArgs`

- `component` — Component name to test
- `skip_lint` — Skip linting before running tests
- `setting` — Override settings as key=value pairs
- `path` — Override local_path for this test run (use a workspace clone or temp checkout)
- `args` — Additional arguments to pass to the test runner (after --)
- `json` — Accept --json for compatibility (output is JSON by default)

### `TransferArgs`

- `source` — Source: local path or server_id:/path
- `destination` — Destination: local path or server_id:/path
- `recursive` — Transfer directories recursively
- `compress` — Compress data during transfer
- `dry_run` — Show what would be transferred without doing it
- `exclude` — Exclude patterns (can be specified multiple times)

### `UpgradeArgs`

- `check` — Check for updates without installing
- `force` — Force upgrade even if already at latest version
- `no_restart` — Skip automatic restart after upgrade
- `method` — Override install method detection (homebrew|cargo|source|binary)
- `json` — Accept --json for compatibility (output is JSON by default)

### `VersionArgs`

- `command`

### `SshResolveArgs`

- `id` — Bare ID (tries project first, then server)
- `project` — Force project resolution
- `server` — Force server resolution

## Subcommands

### `ApiCommand`

- `Get` — Make a GET request
- `endpoint` — API endpoint (e.g., /wp/v2/posts)
- `Post` — Make a POST request
- `endpoint` — API endpoint
- `body` — JSON body
- `Put` — Make a PUT request
- `endpoint` — API endpoint
- `body` — JSON body
- `Patch` — Make a PATCH request
- `endpoint` — API endpoint
- `body` — JSON body
- `Delete` — Make a DELETE request
- `endpoint` — API endpoint

### `AuthCommand`

- `Login` — Authenticate with a project's API
- `project` — Project ID
- `identifier` — Username or email
- `password` — Password (or read from stdin)
- `Logout` — Clear stored authentication for a project
- `project` — Project ID
- `Status` — Show authentication status for a project
- `project` — Project ID

### `ChangelogCommand`

- `Show` — Show a changelog (Homeboy's own if no component specified)
- `component_id` — Component ID to show changelog for
- `Init` — Initialize a new changelog file
- `path` — Path for the changelog file (relative to component)
- `configure` — Also update component config to add changelogTargets
- `component_id` — Component ID

### `ComponentCommand`

- `Create` — Create a new component configuration
- `json` — JSON input spec for create/update (supports single or bulk)
- `skip_existing` — Skip items that already exist (JSON mode only)
- `local_path` — Absolute path to local source directory (ID derived from directory name)
- `remote_path` — Remote path relative to project basePath
- `build_artifact` — Build artifact path relative to localPath
- `version_targets` — Version targets in the form "file" or "file::pattern" (repeatable). For complex patterns, use --version-targets @file.json to avoid shell escaping
- `version_targets_json`
- `extract_command` — Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
- `changelog_target` — Path to changelog file relative to localPath
- `extensions` — Extension(s) this component uses (e.g., "wordpress"). Repeatable.
- `Show` — Display component configuration
- `id` — Component ID
- `Set` — Update component configuration fields  Supports dedicated flags for common fields (e.g., --local-path, --changelog-target) as well as --json for arbitrary updates. When combining --json with dynamic trailing flags, use '--' separator.
- `args`
- `local_path` — Absolute path to local source directory
- `remote_path` — Remote path relative to project basePath
- `build_artifact` — Build artifact path relative to localPath
- `extract_command` — Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
- `changelog_target` — Path to changelog file relative to localPath
- `version_targets` — Version targets in the form "file" or "file::pattern" (repeatable). Same format as `component create --version-target`.
- `extensions` — Extension(s) this component uses (e.g., "wordpress"). Repeatable.
- `Delete` — Delete a component configuration
- `id` — Component ID
- `Rename` — Rename a component (changes ID directly)
- `id` — Current component ID
- `new_id` — New component ID (should match repository directory name)
- `List` — List all available components
- `Projects` — List projects using this component
- `id` — Component ID
- `Shared` — Show which components are shared across projects
- `id` — Specific component ID to check (optional, shows all if omitted)
- `AddVersionTarget` — Add a version target to a component
- `id` — Component ID
- `file` — Target file path relative to component root
- `pattern` — Regex pattern with capture group for version

### `ConfigCommand`

- `Show` — Display configuration (merged defaults + file)
- `builtin` — Show only built-in defaults (ignore homeboy.json)
- `Set` — Set a configuration value at a JSON pointer path
- `pointer` — JSON pointer path (e.g., /defaults/deploy/scp_flags)
- `value` — Value to set (JSON)
- `Remove` — Remove a configuration value at a JSON pointer path
- `pointer` — JSON pointer path (e.g., /defaults/deploy/scp_flags)
- `Reset` — Reset configuration to built-in defaults (deletes homeboy.json)
- `Path` — Show the path to homeboy.json

### `DbCommand`

- `Tables` — List database tables
- `project_id` — Project ID
- `args` — Optional subtarget
- `Describe` — Show table structure
- `project_id` — Project ID
- `args` — Optional subtarget and table name
- `Query` — Execute SELECT query
- `project_id` — Project ID
- `args` — Optional subtarget and SQL query
- `Search` — Search table by column value
- `project_id` — Project ID
- `table` — Table name
- `column` — Column to search
- `pattern` — Search pattern
- `exact` — Use exact match instead of LIKE
- `limit` — Maximum rows to return
- `subtarget` — Optional subtarget
- `DeleteRow` — Delete a row from a table
- `project_id` — Project ID
- `args` — Table name and row ID
- `DropTable` — Drop a database table
- `project_id` — Project ID
- `args` — Table name
- `Tunnel` — Open SSH tunnel to database
- `project_id` — Project ID
- `local_port` — Local port to bind

### `DocsCommand`

- `Map` — Generate a machine-optimized codebase map for AI documentation
- `component_id` — Component to analyze
- `source_dirs` — Source directories to analyze (comma-separated, or repeat flag). Overrides auto-detection.
- `include_private` — Include private methods and internals (default: public API surface only)
- `write` — Write markdown documentation files to disk (default: JSON to stdout)
- `output_dir` — Output directory for markdown files (default: docs)
- `Audit` — Audit documentation for broken links and stale references
- `component_id` — Component ID or direct filesystem path to audit
- `docs_dir` — Docs directory relative to component/project root (overrides config, default: docs)
- `features` — Include full list of all detected features in output

### `ExtensionCommand`

- `List` — Show available extensions with compatibility status
- `project` — Project ID to filter compatible extensions
- `Show` — Show detailed information about a extension
- `extension_id` — Extension ID
- `Run` — Execute a extension
- `extension_id` — Extension ID
- `project` — Project ID (defaults to active project)
- `component` — Component ID (required when ambiguous)
- `input` — Input values as key=value pairs
- `step` — Run only specific steps (comma-separated, e.g. --step phpunit,phpcs)
- `skip` — Skip specific steps (comma-separated, e.g. --skip phpstan,lint)
- `args` — Arguments to pass to the extension (for CLI extensions)
- `stream` — Stream output directly to terminal (default: auto-detect based on TTY)
- `no_stream` — Disable streaming and capture output (default: auto-detect based on TTY)
- `Setup` — Run the extension's setup command (if defined)
- `extension_id` — Extension ID
- `Install` — Install a extension from a git URL or local path
- `source` — Git URL or local path to extension directory
- `id` — Override extension id
- `Update` — Update an installed extension (git pull)
- `extension_id` — Extension ID (omit with --all to update everything)
- `all` — Update all installed extensions
- `force` — Force update even with uncommitted changes
- `Uninstall` — Uninstall a extension
- `extension_id` — Extension ID
- `Action` — Execute a extension action (API call or builtin)
- `extension_id` — Extension ID
- `action_id` — Action ID
- `project` — Project ID (required for API actions)
- `data` — JSON array of selected data rows
- `Exec` — Run a tool from a extension's vendor directory
- `extension_id` — Extension ID
- `component` — Component ID (sets working directory to component path)
- `args` — Command and arguments to run
- `Set` — Update extension manifest fields
- `extension_id` — Extension ID (optional if provided in JSON body)
- `json` — JSON object to merge into manifest (supports @file and - for stdin)
- `replace` — Replace these fields instead of merging arrays

### `FileCommand`

- `List` — List directory contents
- `project_id` — Project ID
- `path` — Remote directory path
- `Read` — Read file content
- `project_id` — Project ID
- `path` — Remote file path
- `raw` — Output raw content only (no JSON wrapper)
- `Write` — Write content to file (from stdin)
- `project_id` — Project ID
- `path` — Remote file path
- `Delete` — Delete a file or directory
- `project_id` — Project ID
- `path` — Remote path to delete
- `recursive` — Delete directories recursively
- `Rename` — Rename or move a file
- `project_id` — Project ID
- `old_path` — Current path
- `new_path` — New path
- `Find` — Find files by name pattern
- `project_id` — Project ID
- `path` — Directory path to search
- `name` — Filename pattern (glob, e.g., "*.php")
- `file_type` — File type: f (file), d (directory), l (symlink)
- `max_depth` — Maximum directory depth
- `Grep` — Search file contents
- `project_id` — Project ID
- `path` — Directory path to search
- `pattern` — Search pattern
- `name` — Filter files by name pattern (e.g., "*.php")
- `max_depth` — Maximum directory depth
- `ignore_case` — Case insensitive search
- `Download` — Download a file or directory from remote server
- `project_id` — Project ID
- `path` — Remote file path
- `local_path` — Local destination path (defaults to current directory)
- `recursive` — Download directories recursively
- `Edit` — Edit file with line-based or pattern-based operations

### `FleetCommand`

- `Create` — Create a new fleet
- `id` — Fleet ID
- `projects` — Project IDs to include (comma-separated or repeated)
- `description` — Description of the fleet
- `Show` — Display fleet configuration
- `id` — Fleet ID
- `Set` — Update fleet configuration
- `args`
- `Delete` — Delete a fleet
- `id` — Fleet ID
- `List` — List all fleets
- `Add` — Add a project to a fleet
- `id` — Fleet ID
- `project` — Project ID to add
- `Remove` — Remove a project from a fleet
- `id` — Fleet ID
- `project` — Project ID to remove
- `Projects` — Show projects in a fleet
- `id` — Fleet ID
- `Components` — Show component usage across a fleet
- `id` — Fleet ID
- `Status` — Show component versions across a fleet (local only)
- `id` — Fleet ID
- `Check` — Check component drift across a fleet (compares local vs remote)
- `id` — Fleet ID
- `outdated` — Only show components that need updates
- `Sync` — [DEPRECATED] Use 'homeboy deploy' instead. See issue #101.
- `id` — Fleet ID
- `category` — Sync only specific categories (repeatable)
- `dry_run` — Show what would be synced without doing it
- `leader` — Override leader server (defaults to fleet-sync.json config)

### `GitCommand`

### `LogsCommand`

- `List` — List pinned log files
- `project_id` — Project ID
- `Show` — Show log file content (shows all pinned logs if path omitted)
- `project_id` — Project ID
- `path` — Log file path (optional - shows all pinned logs if omitted)
- `lines` — Number of lines to show
- `follow` — Follow log output (like tail -f)
- `local` — Execute locally instead of via SSH (for when running on the target server)
- `Clear` — Clear log file contents
- `project_id` — Project ID
- `path` — Log file path
- `local` — Execute locally instead of via SSH
- `Search` — Search log file for pattern
- `project_id` — Project ID
- `path` — Log file path
- `pattern` — Search pattern
- `ignore_case` — Case insensitive search
- `lines` — Limit to last N lines before searching
- `context` — Lines of context around matches
- `local` — Execute locally instead of via SSH

### `ProjectCommand`

- `List` — List all configured projects
- `Show` — Show project configuration
- `project_id` — Project ID
- `Create` — Create a new project
- `json` — JSON input spec for create/update (supports single or bulk)
- `skip_existing` — Skip items that already exist (JSON mode only)
- `id` — Project ID (CLI mode)
- `domain` — Public site domain (CLI mode)
- `server_id` — Optional server ID
- `base_path` — Optional remote base path
- `table_prefix` — Optional table prefix
- `Set` — Update project configuration fields
- `args`
- `Remove` — Remove items from project configuration arrays
- `project_id` — Project ID (optional if provided in JSON body)
- `spec` — JSON spec (positional, supports @file and - for stdin)
- `json` — Explicit JSON spec (takes precedence over positional)
- `Rename` — Rename a project (changes ID)
- `project_id` — Current project ID
- `new_id` — New project ID
- `Components` — Manage project components
- `command`
- `Pin` — Manage pinned files and logs
- `command`
- `Delete` — Delete a project configuration
- `project_id` — Project ID

### `ProjectComponentsCommand`

- `List` — List associated components
- `project_id` — Project ID
- `Set` — Replace project components with the provided list
- `project_id` — Project ID
- `component_ids` — Component IDs
- `Add` — Add one or more components
- `project_id` — Project ID
- `component_ids` — Component IDs
- `Remove` — Remove one or more components
- `project_id` — Project ID
- `component_ids` — Component IDs
- `Clear` — Remove all components
- `project_id` — Project ID

### `ProjectPinCommand`

- `List` — List pinned items
- `project_id` — Project ID
- `Add` — Pin a file or log
- `project_id` — Project ID
- `path` — Path to pin (relative to basePath or absolute)
- `label` — Optional display label
- `tail` — Number of lines to tail (logs only)
- `Remove` — Unpin a file or log
- `project_id` — Project ID
- `path` — Path to unpin

### `RefactorCommand`

- `Rename` — Rename a term across the codebase with case-variant awareness
- `from` — Term to rename from
- `to` — Term to rename to
- `component` — Component ID (uses its local_path as the root)
- `path` — Directory path to refactor (alternative to --component)
- `scope` — Scope: code, config, all (default: all)
- `literal` — Exact string matching (no boundary detection, no case variants)
- `write` — Apply changes to disk (default is dry-run)

### `ServerCommand`

- `Create` — Register a new SSH server
- `json` — JSON input spec for create/update (supports single or bulk)
- `skip_existing` — Skip items that already exist (JSON mode only)
- `id` — Server ID (CLI mode)
- `host` — SSH host
- `user` — SSH username
- `port` — SSH port (default: 22)
- `Show` — Display server configuration
- `server_id` — Server ID
- `Set` — Modify server settings
- `args`
- `Delete` — Remove a server configuration
- `server_id` — Server ID
- `List` — List all configured servers
- `Key` — Manage SSH keys

### `KeyCommand`

- `Generate` — Generate a new SSH key pair and set it for this server
- `server_id` — Server ID
- `Show` — Display the public SSH key
- `server_id` — Server ID
- `Import` — Import an existing SSH private key and set it for this server
- `server_id` — Server ID
- `private_key_path` — Path to private key file
- `Use` — Use an existing SSH private key file path for this server
- `server_id` — Server ID
- `private_key_path` — Path to private key file
- `Unset` — Unset the server SSH identity file (use normal SSH resolution)
- `server_id` — Server ID

### `SshSubcommand`

- `List` — List configured SSH server targets

### `StackCommand`

- `List` — List all installed stack specs
- `Show` — Show a stack spec
- `stack_id` — Stack ID
- `Create` — Create a new stack spec
- `stack_id` — Stack ID
- `component` — Component identifier
- `component_path` — Local checkout path
- `base` — Upstream ref as `<remote>/<branch>`
- `target` — Target branch as `<remote>/<branch>`
- `description` — Optional human-readable description
- `AddPr` — Append a PR entry to a stack
- `stack_id` — Stack ID
- `repo` — GitHub repo coordinate (`owner/name`)
- `number` — PR number
- `note` — Optional human-readable note
- `RemovePr` — Remove a PR entry from a stack
- `stack_id` — Stack ID
- `number` — PR number
- `repo` — Optional repo disambiguator
- `Apply` — Materialize a stack target branch
- `stack_id` — Stack ID
- `Status` — Report upstream PR state and local target state
- `stack_id` — Stack ID
- `Sync` — Rebuild target and auto-drop merged PRs
- `stack_id` — Stack ID
- `dry_run` — Preview without mutating the spec or target branch
- `Inspect` — Inspect the current branch as a stack of commits
- `component_id` — Optional component ID, auto-detected from CWD when omitted
- `base` — Base ref to compare against
- `no_pr` — Skip GitHub PR lookup
- `repo` — Scope PR lookup to one GitHub repo
- `path` — Workspace path override

### `VersionCommand`

- `Show` — Show current version (default: homeboy binary)
- `component_id` — Component ID (optional - shows homeboy binary version when omitted)
- `path` — Override local_path for version file lookup
- `Set` — [DEPRECATED] Use 'homeboy version bump' or 'homeboy release' instead. See issue #259.
- `component_id` — Component ID
- `new_version` — New version (e.g., 1.2.3)
- `path` — Override local_path for version file lookup
- `Bump` — Bump version with semantic versioning (alias for `release`)
- `component_id` — Component ID
- `bump_type` — Version bump type (patch, minor, major)
- `dry_run` — Preview what will happen without making changes
- `path` — Override local_path for version operations

### `Commands`

- `Project` — Manage project configuration
- `Ssh` — SSH into a project server or configured server
- `Server` — Manage SSH server configurations
- `Test` — Run tests for a component
- `Bench` — Run performance benchmarks for a component
- `Lint` — Lint a component
- `Db` — Database operations
- `File` — Remote file operations
- `Fleet` — Manage fleets (groups of projects)
- `Logs` — Remote log viewing
- `Transfer` — Transfer files between servers
- `Deploy` — Deploy components to remote server
- `Component` — Manage standalone component configurations
- `Config` — Manage global Homeboy configuration
- `Extension` — Execute CLI-compatible extensions
- `Init` — Deprecated alias for `status --full`
- `Status` — Actionable component status overview (`--full` for the full context report)
- `Docs` — Display CLI documentation
- `Changelog` — Changelog operations
- `Git` — Git operations for components
- `Issues` — Reconcile findings against an issue tracker
- `Version` — Version management for components
- `Build` — Build a component
- `Validate` — Validate that code compiles/parses through extension scripts
- `Changes` — Show changes since last version tag
- `Release` — Plan release workflows
- `Review` — Run scoped audit + lint + test umbrella against PR-style changes
- `Audit` — Audit code conventions and detect architectural drift
- `Refactor` — Structural refactoring (rename terms across codebase)
- `Rig` — Manage local dev rigs
- `Stack` — Manage combined-fixes branch stacks
- `Undo` — Undo the last write operation when a snapshot exists
- `Auth` — Authenticate with a project's API
- `Api` — Make API requests to a project
- `Upgrade` — Upgrade Homeboy to the latest version
- `Update` — Alias for upgrade
- `List` — List available commands (alias for --help)
