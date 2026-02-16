# Changelog

All notable changes to Homeboy CLI are documented in this file.

(This file is embedded into the CLI binary and is also viewable via `homeboy changelog`.)

## [0.44.4] - 2026-02-16

### Fixed
- SSH non-interactive commands now use BatchMode, ConnectTimeout, and ServerAliveInterval to prevent hangs (#88)
- Version target patterns are validated at create time — rejects template syntax and missing capture groups (#90)
- component set now supports --version-target flag like component create (#91)

## [0.44.3] - 2026-02-15

### Fixed
- version bump: run pre_version_bump_commands after bump to keep generated artifacts (e.g. Cargo.lock) in the release commit
- deploy: upload to temp file + atomic mv to avoid scp 'Text file busy' when replacing running binaries

## [0.44.2] - 2026-02-15

### Fixed
- ssh: allow multi-arg non-interactive commands; improve non-TTY guidance

## [0.44.1] - 2026-02-15

### Fixed
- Update Cargo.lock after 0.44.0 release

## [0.44.0] - 2026-02-14

### Added
- Fleet sync command (homeboy fleet sync) — sync OpenClaw agent configs, skills, and tools across fleet servers with manifest-driven categories, JSON merging, auto-detection of OpenClaw home paths, ownership fixing, and dry-run support

## [0.43.1] - 2026-02-13

### Fixed
- Handle uncommitted changelog gracefully in version bump (#78)

- fix: scope --allow-root injection to wordpress module only
- Better error message for missing unreleased changelog section
- Revert Stdio::null on git commands (broke HTTPS credential helper)

## [0.43.0] - 2026-02-13

### Added
- Support aliases for components, projects, and servers (#34)
- Detect and warn about outdated modules in homeboy init (#26)
- Automatic retry with backoff for transient SSH failures (#51)
- Release --recover for interrupted releases (#38)
- Git-based deployment strategy (#52)

### Fixed
- Clarify local file permissions message with path and chmod modes (#9)
- Expand {{module_path}} in project CLI command templates (#44)
- Fix environment-dependent docs audit test

## [0.42.0] - 2026-02-13

### Added
- support aliases for components, projects, and servers
- add transfer command for server-to-server file transfer (#67)
- add file download command (SCP remote-to-local)
- add class name detection to audit, fix scaffold false positives, document generate spec

### Fixed
- use non-existent path in docs audit test
- expand {{module_path}} in project CLI command templates
- clarify local file permissions message with path and modes

## [0.41.2] - 2026-02-10

### Added
- Cross-compilation guide documenting platform requirements

## [0.41.1] - 2026-02-10

### Added
- OpenClaw skill for AI agent usage (skills/homeboy/)

## [0.41.0] - 2026-02-10

### Added
- Fleet management: create, list, show, delete, add, remove projects from fleets
- fleet status: check versions across all projects in a fleet
- fleet check: drift detection across fleet using deploy --check
- deploy --fleet: deploy component to all projects in a fleet
- deploy --shared: deploy to all projects using a component (auto-detect)
- component shared: show which projects use a component

## [0.40.4] - 2026-02-10

### Added
- Module manifest: add Desktop runtime fields (dependencies, playwrightBrowsers, builtin actions)

### Fixed
- Parser: trim content in replace_all to match extract_all behavior (fixes version bump on files with trailing newlines)

## [0.40.3] - 2026-02-09

- Add cargo-dist release workflow for automatic homebrew tap updates

## [0.40.2] - 2026-02-09

### Added
- agnostic source directory detection for scaffold (#57)

## [0.40.1] - 2026-02-03

### Added
- add preflight remote sync check to version bump to prevent push conflicts

### Fixed
- source cargo env for source installs

## [0.40.0] - 2026-02-02

### Added
- filter merge commits from changelog auto-generation
- add --projects flag for multi-project deployment

## [0.39.5] - 2026-02-01

- inject --allow-root for root SSH deploy overrides

## [0.39.4] - 2026-01-31

### Added
- auto-inject --allow-root for root SSH users

## [0.39.3] - 2026-01-31

### Added
- support glob patterns in build_artifact

## [0.39.2] - 2026-01-31

### Added
- capture command output in JSON response

## [0.39.1] - 2026-01-28

### Added
- Display human-readable success summary after version bump/release
- Transform docs-audit from link checker to content alignment tool

## [0.39.0] - 2026-01-28

- add ValidationCollector for aggregated error reporting in version bump

## [0.38.6] - 2026-01-28

### Added
- validate conflicting version targets for same file
- add --fix flag for auto-fixing lint issues

### Fixed
- fix(docs-audit): filter false positives via module-level ignore patterns

## [0.38.5] - 2026-01-28

- Fixing my fuck-up with version bumping

## [0.38.4] - 2026-01-28

- Make documentation guidance audit-driven with concrete commands

## [0.38.3] - 2026-01-28

- Stream test/lint output directly to terminal instead of capturing in JSON

## [0.38.2] - 2026-01-27

- Fix version bump race condition where changelog was finalized before all version targets were validated, causing 'No changelog items found' on retry after validation failure

## [0.38.1] - 2026-01-26

- Add flag-style aliases for version and changelog commands (#13, #32)

## [0.38.0] - 2026-01-26

### Added
- auto-generate changelog entries from conventional commits (#25)

## [0.37.5] - 2026-01-26

### Added
- Add --base64 flag to component/server set commands to bypass shell escaping (#24)

### Fixed
- Fix quote-aware argument splitting in normalize_args() for WP-CLI eval commands (#30)

## [0.37.4] - 2026-01-26

### Fixed
- Add --component option alias for changelog add (#32)

## [0.37.3] - 2026-01-26

### Fixed
- Graceful version bump when changelog already finalized for target version

## [0.37.2] - 2026-01-26

### Fixed
- Case-insensitive enum arguments for --type and BUMP_TYPE (closes #29)

## [0.37.1] - 2026-01-26

### Fixed
- Allow uncommitted changelog and version files during release (fixes #28)

## [0.37.0] - 2026-01-25

- Add configurable lint and test script paths via module manifest (lint.module_script, test.module_script)

## [0.36.4] - 2026-01-24

### Removed
- Remove --force flag from version bump and release commands (bypassing validation defeats its purpose)

## [0.36.3] - 2026-01-23

- Add success_summary to pipeline output for human-readable release summaries

## [0.36.2] - 2026-01-23

- Fix error message visibility in internal_unexpected errors

## [0.36.1] - 2026-01-23

### Added
- Add changelog entry awareness to changes command

## [0.36.0] - 2026-01-23

- feat: distinguish docs-only commits from code changes in init command (#16)

## [0.35.1] - 2026-01-23

- Add clean working tree hint to changelog validation errors

## [0.35.0] - 2026-01-23

- feat: entity suggestion for unrecognized subcommands

## [0.34.1] - 2026-01-22

- fix: require clean working tree for version bump (removes pre-release commit behavior)

## [0.34.0] - 2026-01-22

- Add shared project/component argument resolution primitive (utils/resolve.rs)
- Add project-level build support with --all flag
- Support flexible argument order in changes command
- Add hooks system documentation
- Update agent system reminder wording

## [0.33.12] - 2026-01-22

- feat: add module-defined CLI help configuration

## [0.33.11] - 2026-01-22

- fix: normalize quoted CLI args at entry point (closes #11)

## [0.33.10] - 2026-01-21

- Add post_release_commands support to release pipeline

## [0.33.9] - 2026-01-21

- Add context-aware component suggestions for version bump command

## [0.33.8] - 2026-01-21

- feat: Add project:subtarget colon syntax for CLI tools (both 'extra-chill:events' and 'extra-chill events' now work)

## [0.33.7] - 2026-01-21

- fix: is_workdir_clean() now correctly identifies clean repositories (fixes #6)

## [0.33.6] - 2026-01-21

### Added
- Add `component add-version-target` command for adding version targets without full JSON spec

### Changed
- Auto-insert `--` separator for trailing_var_arg commands (`component set`, `server set`, `test`) - intuitive syntax now works without explicit separator

## [0.33.5] - 2026-01-21

- Create engine/ directory with pipeline and executor modules
- Move base_path.rs and slugify.rs to utils/

## [0.33.4] - 2026-01-21

- Remove ReleaseConfig - publish targets now derived purely from modules with release.publish action

## [0.33.3] - 2026-01-21

- Fix publish step module lookup by parsing prefix once in from_str (single source of truth)
- Add cleanup step to release pipeline to remove target/distrib/ after publish

## [0.33.2] - 2026-01-21

- **Release Pipeline**: Fixed architecture to use module's `release.package` action for artifact creation instead of direct build

## [0.33.1] - 2026-01-21

- fix: add missing Build step to release pipeline

## [0.33.0] - 2026-01-21

- Refactor release system: built-in core steps (commit, tag, push) with config-driven publish targets

## [0.32.7] - 2026-01-21

- Fix release config-first: component release.steps now respected instead of overwritten with generated defaults
- Remove --no-tag, --no-push, --no-commit flags from release command (use git primitives for partial workflows)

## [0.32.6] - 2026-01-21

- Add --deploy flag to release command for automatic deployment to all projects using the component
- Add --force flag to deploy command to allow deployment with uncommitted changes
- Fix version commit detection to recognize 'Version X.Y.Z' and 'Version bump to X.Y.Z' commit formats

## [0.32.5] - 2026-01-20

- Add 'homeboy module show' command for detailed module inspection

## [0.32.4] - 2026-01-20

- Add build-time local_path validation with clear error messages
- Add tilde expansion (~/) support for component local_path
- Add gap_details to init output for inline config gap explanations
- Add project auto-detection for deploy when only component ID provided
- Add normalize_args() to handle both quoted and unquoted CLI tool arguments

## [0.32.3] - 2026-01-20

- Consolidate release runner, fix step ordering

## [0.32.2] - 2026-01-20

### Added
- Add validate_local_path with self-healing hints for misconfigured components

## [0.32.1] - 2026-01-20

### Refactored
- Refactor release module into cleaner module structure

## [0.32.0] - 2026-01-20

### Added
- Add `version bump` command as alias for release (e.g., `homeboy version bump homeboy minor`)
- Add `--no-commit` flag to release command to skip auto-committing uncommitted changes
- Add `--commit-message` flag to release command for custom pre-release commit messages
- Add version show shorthand: `homeboy version <component>` now works as `homeboy version show <component>`

### Changed
- Release command now auto-commits uncommitted changes by default (use `--no-commit` to opt-out)
- Improve build verification before release

## [0.31.1] - 2026-01-20

- Consolidate I/O primitives and option chains for cleaner code

## [0.31.0] - 2026-01-20

### Added
- Add release command flags: --dry-run (preview), --local (skip push/publish), --publish (force full pipeline), --no-tag, --no-push

### Changed
- Unify release command: 'homeboy release <component> <patch|minor|major>' now handles version bump, commit, tag, and optional push/publish in one flow

### Removed
- Remove 'version bump' command - use 'homeboy release <component> patch|minor|major' instead
- Remove 'release run' and 'release plan' subcommands - use 'homeboy release <component> patch|minor|major [--dry-run]' instead

## [0.30.16] - 2026-01-20

### Added
- Add --project/-p flag to deploy command for explicit project specification

### Refactored
- Add utils/io module with read_file and write_file helpers for consistent error handling

### Refactored
- Add json_path_str helper for nested JSON value extraction

## [0.30.15] - 2026-01-20

### Added
- Add Refactored changelog entry type with Refactor alias
- Add stage_files function for targeted git staging operations
- Auto-stage changelog changes before version bump clean-tree check
- Add lines_to_vec helper for common string-to-vec-lines pattern

### Changed
- Replace manual error checking with validation helper utilities across codebase
- Use String::from instead of .to_string() for owned string conversions

### Fixed
- Improve orphaned tag auto-fix messaging in release pipeline

## [0.30.14] - 2026-01-20

### Changed
- Consolidate utils and create command primitives

### Fixed
- Fix changelog init --configure circular error
- Accept changelog_targets as alias for changelog_target

## [0.30.13] - 2026-01-20

- Auto-fix orphaned tags in git.tag step instead of failing with hints

## [0.30.12] - 2026-01-20

- Add pre_version_bump_commands for staging build artifacts before clean-tree check
- Improve orphaned tag hint with one-liner fix command
- Enhance version bump commit failure error with recovery guidance

## [0.30.11] - 2026-01-20

- Migrate changelog, init, and deploy to use parser utilities for version extraction and path resolution

## [0.30.10] - 2026-01-20

- Wire up version-aware baseline detection in changes() to fix stale tag mismatch
- Add unconfigured version pattern detection to init warnings
- Clarify init command help text and documentation

## [0.30.9] - 2026-01-20

### Added
- Added: Comprehensive schema, architecture, and developer guide documentation

## [0.30.8] - 2026-01-20

- Make release git.tag step idempotent to work with version bump tags
- Add release pipeline hint after version bump tagging

## [0.30.7] - 2026-01-20

### Changed
- Improve version bump error hints to explain why working tree must be clean

## [0.30.6] - 2026-01-20

### Added
- Require clean working tree before version bump with helpful hints

## [0.30.5] - 2026-01-20

### Added
- Add automatic git tag creation after version bump commits

## [0.30.4] - 2026-01-20

- Accept --json flag as no-op on commands that return JSON by default (init, test, lint, release, upgrade)

## [0.30.3] - 2026-01-20

- Add plural aliases for entity commands (servers, components, modules)

## [0.30.2] - 2026-01-20

- Fixed: Version baseline detection now correctly identifies stale tags and falls back to release commits for accurate commit counts

## [0.30.1] - 2026-01-20

### Added
- Added: `status` alias for `init` command

### Removed
- Removed: `context` command (use `init` instead)

## [0.30.0] - 2026-01-19

- Added component auto-detection in `homeboy changes` - auto-uses detected component when exactly one matched
- Added version/baseline alignment warning in `homeboy init` when source file version differs from git baseline
- Renamed `GitSnapshot.version_baseline` to `baseline_ref` for consistency with `changes` output

## [0.29.3] - 2026-01-19

- Remove redundant fields from init JSON output (context.contained_components, context.components, context.command)
- Add gaps field to components array in init output for parent context
- Make version block conditional on managed context in init output
- Skip empty settings HashMap serialization in module configs
- Skip null suggestion field serialization in context output

## [0.29.2] - 2026-01-19

- Add per-component release_state to init output (commits_since_version, has_uncommitted_changes, baseline_ref)

## [0.29.1] - 2026-01-19

- Add --status as visible alias for deploy --check

## [0.29.0] - 2026-01-19

- Add docs audit subcommand for link validation and staleness detection
- Change docs scaffold to require component_id for consistency with other commands
- Fix docs topic parsing to not consume flags as part of topic path
- Add agent_context_files to init output showing git-tracked markdown files

## [0.28.1] - 2026-01-19

- Add capability hints to lint and test commands for better discoverability

## [0.28.0] - 2026-01-19

- Add release state tracking to init and deploy --check for detecting unreleased work

## [0.27.13] - 2026-01-19

- Fix passthrough arguments documentation to be generic

## [0.27.12] - 2026-01-19

- Add shell quoting documentation to wp command docs
- Display subtargets in homeboy init output for project discoverability
- Support both argument orders for deploy command (project-first or component-first)
- Add CLI tool suggestions to homeboy init next_steps when modules have CLI tools

## [0.27.11] - 2026-01-19

### Added
- Added lint summary header showing error/warning counts at top of output
- Added --sniffs, --exclude-sniffs, and --category flags for lint filtering

### Changed
- Enhanced --summary to show top violations by sniff type

### Fixed
- Fixed custom fixers ignoring --file and --glob targets

## [0.27.10] - 2026-01-19

### Added
- Add --level flag as alternative to positional bump type in version bump command

### Fixed
- Make --changed-only flag language-agnostic (removes hardcoded .php filter)

## [0.27.9] - 2026-01-19

### Added
- Add --changed-only flag to lint command for focusing on modified PHP files
- Add prerequisites validation to release plan (warns about empty changelog)

## [0.27.8] - 2026-01-19

### Fixed
- Pass HOMEBOY_MODULE_PATH environment variable to build commands

## [0.27.7] - 2026-01-19

### Fixed
- Fixed: version set no longer validates/finalizes changelog (version-only operation)
- Fixed: version show now displays all configured version targets, not just the primary

## [0.27.6] - 2026-01-19

- Fixed: settings_flags now applied during direct execution for local CLI tools

## [0.27.5] - 2026-01-19

### Added
- Add ModuleRunner builder for unified test/lint script orchestration
- Add ReleaseStepType enum for typed release pipeline steps

### Changed
- Refactor lint and test commands to use ModuleRunner, reducing code duplication
- Simplify deploy, version, and SSH commands with shared utilities

## [0.27.4] - 2026-01-18

### Added
- Immediate 'homeboy is working...' feedback for TTY sessions

## [0.27.3] - 2026-01-18

### Security
- Fix heredoc injection vulnerability in file write operations
- Fix infinite loop in pattern replacement when pattern appears in replacement
- Fix grep failing on single files (was always using recursive flag)
- Fix non-portable --max-depth in grep (now uses find|xargs)
- Fix race condition in file prepend operations (now uses mktemp)
- Fix inconsistent echo behavior in append/prepend (now uses printf)

### Added
- Add --raw flag to `file read` for output without JSON wrapper

### Changed
- Separate stdout/stderr in lint and test command output

## [0.27.2] - 2026-01-18

- Add granular lint options: --file, --glob, and --errors-only flags for targeted linting

## [0.27.1] - 2026-01-18

- Add --summary flag to lint command for compact output

## [0.27.0] - 2026-01-18

- feat: make build_artifact optional—modules can provide artifact_pattern for automatic resolution
- feat: deploy command supports --project flag as alternative to positional argument
- feat: context gaps now detect missing buildArtifact when remotePath is configured
- fix: version parsing now trims content for VERSION files with trailing newlines
- docs: comprehensive README overhaul with workflow examples and module system documentation

## [0.26.7] - 2026-01-18

- Add `homeboy lint` command for standalone code linting via module scripts
- Add `--skip-lint` flag to `homeboy test` to run tests without linting
- Add `pre_build_script` hook to module BuildConfig for pre-build validation

## [0.26.6] - 2026-01-18

### Added
- NullableUpdate<T> type alias for three-state update semantics in CLI commands

### Changed
- refactor module.rs into module/ directory with focused submodules (manifest, execution, scope, lifecycle, exec_context)
- replace .unwrap() calls with .expect() for safer error handling across codebase
- extract duplicate template variable building into DbContext::base_template_vars()
- unify scp_file and scp_recursive into shared scp_transfer() function
- use OnceLock for lazy regex compilation in template resolution

### Fixed
- load_all_modules() calls now use unwrap_or_default() to handle errors gracefully

## [0.26.5] - 2026-01-18

- feat: add --stream and --no-stream flags to module run command for explicit output control
- feat: add HOMEBOY_COMPONENT_PATH environment variable to test runners
- feat: make ModuleExecutionMode enum public for module integration

## [0.26.4] - 2026-01-18

- feat: new test command for running component test suites with module-based infrastructure

## [0.26.3] - 2026-01-18

- feat: enhanced module list JSON output with CLI tool info, available actions, and runtime status flags
- feat: added context-aware error hints suggesting 'homeboy init' when project context is missing

## [0.26.2] - 2026-01-18

- Test dry-run validation

## [0.26.1] - 2026-01-18

### Fixed
- version bump command now accepts bump type as positional argument without requiring -- separator

## [0.26.0] - 2026-01-18

### Added
- Added: automatic docs topic resolution with fallback prefixes for common shortcuts (e.g., 'version' → 'commands/version', 'generation' → 'documentation/generation')

### Changed
- Changed: config directory moved to universal ~/.config/homeboy/ on all platforms (previously ~/Library/Application Support/homeboy on macOS). Users may need to migrate config files manually.

## [0.25.4] - 2026-01-18

- Fixed: changelog init now checks for existing changelog files before creating new ones, preventing duplicates

## [0.25.1] - 2026-01-17

- Enforce changelog hygiene: version set/bump require clean changelog, release rejects unreleased content

## [0.25.0] - 2026-01-17

### Fixed
- Require explicit subtarget when project has subtargets configured, preventing unintended main site operations in multisite networks

## [0.24.3] - 2026-01-17

- feat: homeboy version show defaults to binary version when no component_id provided

## [0.24.2] - 2026-01-17

- fix: upgrade restart command now uses --version instead of version show to avoid component_id error

## [0.24.1] - 2026-01-17

- fix: Improve error message when `homeboy changes` runs without component ID

## [0.24.0] - 2026-01-17

- feat: Add module-provided build script support with priority-based command resolution

## [0.23.0] - 2026-01-16

- feat: Add settings_flags to CLI modules for automatic flag injection from project settings

## [0.22.10] - 2026-01-16

- fix: Release pipeline always creates annotated tags ensuring git push --follow-tags works correctly

## [0.22.9] - 2026-01-16

### Fixed
- Release pipeline amends previous release commit instead of creating duplicates

## [0.22.8] - 2026-01-16

- fix: release pipeline pushes commits with tags and skips duplicate commits

## [0.22.7] - 2026-01-16

- Make path optional in logs show - shows all pinned logs when omitted

## [0.22.6] - 2026-01-16

- Add changelog show subcommand with optional component_id support

## [0.22.5] - 2026-01-16

- Allow `homeboy release <component>` as shorthand for `homeboy release run <component>`

## [0.22.4] - 2026-01-16

- Support --patch/--minor/--major flag syntax for version bump command

## [0.22.3] - 2026-01-16

### Added
- Add --type flag to changelog add command for Keep a Changelog subsection placement

### Fixed
- Improve deploy error message when component ID provided instead of project ID

## [0.22.2] - 2026-01-16

- Add --changelog-target flag to component create command
- Make build_artifact and remote_path optional in component create for library projects
- Improve git.tag error handling with contextual hints for tag conflicts

## [0.22.1] - 2026-01-16

- Update documentation to remove all --cwd references

## [0.22.0] - 2026-01-16

- **BREAKING**: Remove `--cwd` flag entirely from CLI - component IDs are THE way to use Homeboy (decouples commands from directory location)
- **BREAKING**: `version bump` now auto-commits version changes. Use `--no-commit` to opt out.
- Add `--dry-run` flag to `version bump` for simulating version changes
- Add changelog warning when Next section is empty during version bump
- Add template variable syntax support for both `{var}` and `{{var}}` in extract commands
- Add deploy override visibility in dry-run mode with "Would..." messaging
- Create unified template variables reference documentation

## [0.21.0] - 2026-01-16

- Add generic module-based deploy override system for platform-specific install commands
- Add `heck` crate for automatic camelCase/snake_case key normalization in config merges
- Fix SIGPIPE panic when piping CLI output to commands like `head`
- Fix `success: true` missing from component set single-item responses
- Fix deploy error messages to include exit code and fall back to stdout when stderr is empty

## [0.20.9] - 2026-01-15

- Omit empty Unreleased section when finalizing releases

## [0.20.8] - 2026-01-15

- Add init snapshots for version, git status, last release, and changelog preview
- Surface module readiness details with failure reason and output
- Omit empty Unreleased section when finalizing releases

## [0.20.7] - 2026-01-15

- Add -m flag for changelog add command (consistent with git commit/tag)
- Support bulk changelog entries via repeatable -m flags
- Add git.tag and git.push steps to release pipeline

## [0.20.6] - 2026-01-15

- add init next_steps guidance for agents

## [0.20.5] - 2026-01-15

- Add git.commit as core release step (auto-inserted before git.tag)
- Add pre-flight validation to fail early on uncommitted changes
- Add PartialSuccess pipeline status with summary output
- Remove GitHub Actions release workflow (replaced by local system)

## [0.20.4] - 2026-01-15

- Add release workflow guidance across docs and README
- Expose database template vars for db CLI commands

## [0.20.3] - 2026-01-15

- **Release system now fully replaces GitHub Actions** - Complete local release pipeline with package, GitHub release, Homebrew tap, and crates.io publishing
- Fix module template variable to use snake_case convention (`module_path`)
- Fix macOS bash 3.x compatibility in module publish scripts (replace `readarray` with POSIX `while read`)
- Add `dist-manifest.json` to .gitignore for cleaner working directory

## [0.20.2] - 2026-01-15

- Prepare release pipeline for module-driven publishing

## [0.20.1] - 2026-01-15

- Fix release pipeline executor and module action runtime

## [0.20.0] - 2026-01-15

- Add parallel pipeline planner/executor for releases
- Add component-scoped release planner and runner
- Support module actions for release payloads and command execution
- Add module-driven release payload context (version/tag/notes/artifacts)
- Add git include/exclude file scoping
- Add config replace option for set commands
- Improve changelog CLI help and detection

## [0.19.3] - 2026-01-15

- Remove agent-instructions directory - docs are the single source of truth
- Simplify build.rs to only embed docs/
- Update README with streamlined agent setup instructions

## [0.19.2] - 2026-01-15

- Add post_version_bump_commands hook to run commands after version bumps
- Run cargo publish with --locked to prevent lockfile drift in releases

## [0.19.1] - 2026-01-15

- fix: `homeboy changes` surfaces noisy untracked hints and respects `.gitignore`

## [0.19.0] - 2026-01-15

- feat: add `homeboy config` command for global configuration
- feat: configurable SCP flags, permissions, version detection patterns
- feat: configurable install method detection and upgrade commands
- fix: `homeboy docs` uses raw markdown output only, removes --list flag

## [0.18.0] - 2026-01-15

- Add belt & suspenders permission fixing (before build + after extraction)
- Add -O flag for SCP legacy protocol compatibility (OpenSSH 9.x)
- Add verbose output for deploy steps (mkdir/upload/extract)
- Add SSH auto-cd to project base_path when project is resolved
- Fix changelog finalization error propagation with helpful hints
- Inherit changelog settings from project when component has single project

## 0.17.0

- Agnostic local/remote command execution - db, logs, files now work for local projects
- Init command returns structured JSON with context, servers, projects, components, and modules
- New executor.rs provides unified command routing based on project config
- Renamed remote_files module to files (environment-agnostic)

## 0.16.0

- **BREAKING**: JSON output now uses native snake_case field names (e.g., project_id, server_id, base_path)
- Remove all serde camelCase conversion annotations
- Consolidate json module into config and output modules

## 0.15.0

- Added bulk merge support for component/project/server set commands
- Improved coding-agent UX: auto-detect commit message vs JSON, better fuzzy matching, and fixed --cwd parsing
- Refactored create flow into a single unified function
- Removed dry-run mode and related behavior
- Improved auto-detection tests
- Included pending context and documentation changes

## 0.14.0

- Merge workspace into single crate for crates.io publishing
- Add src/core/ architectural boundary separating library from CLI
- Library users get ergonomic imports via re-exports (homeboy::config instead of homeboy::core::config)

## 0.13.0

- Add --staged-only flag to git commit for committing only pre-staged changes
- Add --files flag to git commit for staging and committing specific files
- Add commit_from_json() for unified JSON input with auto-detect single vs bulk format
- Align git commit JSON input pattern with component set (positional spec, stdin, @file support)

## 0.12.0

- Add `homeboy upgrade` command for self-updates
- Improve `homeboy context` output for monorepo roots (show contained components)
- Fix `homeboy changes` single-target JSON output envelope
- Clarify recommended release workflow in docs

## 0.11.0

- Add universal fuzzy matching for entity not-found errors
- Align changes output examples with implementation

## 0.10.0

- Refactor ID resolution and standardize resolving IDs from directory names
- Add `homeboy module set` to merge module manifest JSON
- Centralize config entity rename logic
- Refactor project pin/unpin API with unified options

## 0.9.0

- Add remote find and grep commands for server file search
- Add helpful hints to not-found error messages
- Refactor git module for cleaner baseline detection
- Add slugify module
- Documentation updates across commands

## 0.8.0

- Refactor JSON output envelope (remove warnings payload; simplify command JSON mapping)
- Unify bulk command outputs under BulkResult/ItemOutcome with success/failure summaries
- Remove per-project module enablement checks; use global module manifests for build/deploy/db/version defaults
- Deploy output: rename components -> results and add total to summary

## 0.7.5

- Fix Homebrew formula name: cargo-dist now generates homeboy.rb instead of homeboy-cli.rb

## 0.7.4

- Update skill documentation with changelog ops, version set, and bulk JSON syntax
- Support positional component filtering in changes command

## 0.7.3

- Support positional message argument for changelog add and git commit commands
- Add version set command for direct version assignment

## 0.7.2

- Add tiered fallback for changes command when no tags exist (version commits → last 10 commits)

## 0.7.1

- Align homeboy init docs source with agent-instructions
- Simplify changelog add --json format to match other bulk commands

## 0.7.0

- Refactor CLI commands to delegate business logic to the core library
- Add core git module for component-scoped git operations
- Add core version module for version target read/update utilities
- Improve changes command output for local working tree state
- Refresh embedded CLI docs and JSON output contract

## 0.6.0

- Add universal --merge flag for component/project/server set commands
- Fix changelog entry spacing to preserve blank line before next version
- Refactor core into a headless/public API; treat the CLI as one interface
- Move business logic into the `homeboy` core library and reduce CLI responsibilities
- Standardize command/output layers and keep TTY concerns in the CLI
- Introduce/expand the module system and module settings
- Add generic auth support plus a generic API client/command
- Remove/adjust doctor and error commands during stabilization

## 0.5.0

- Refactor deploy to use a generic core implementation
- Replace component isNetwork flag with extractCommand for post-upload extraction
- Unify module runtime config around runCommand/setupCommand/readyCheck/env and remove plugin-specific fields
- Update docs and examples for new generic deployment and module behavior

## 0.4.1

- Rename plugin terminology to module across CLI/docs
- Remove active project concept; require explicit --project where needed
- Update module manifest filename to `<module_id>.json`

## 0.4.0

- Unify plugins and modules under a single module manifest and config surface
- Remove plugin command and plugin manifest subsystem; migrate CLI/db/deploy/version/build to module-based lookups
- Rename config fields: plugins→modules, plugin_settings→module_settings, modules→scoped_modules (superseded by modules field in current releases)

## 0.3.0

- Add plugin support (nodejs/wordpress)
- Add plugin command and plugin manifest integration
- Improve deploy/build/version command behavior and outputs

## 0.2.19

- Fix inverted version validation condition to prevent gaps instead of blocking valid bumps

## 0.2.18

- Fix shell argument escaping for wp and pm2 commands with special characters
- Centralize shell escaping in shell.rs module with quote_arg, quote_args, quote_path functions
- Fix unescaped file paths in logs and file commands
- Remove redundant escaping functions from template.rs, ssh/client.rs, and deploy.rs

## 0.2.17

- Add project set --component-ids to replace component attachments
- Add project components add/remove/clear subcommands
- Add tests for project component attachment workflows

## 0.2.15

- Derive git tag name
- Internal refactor

## 0.2.14

- Fix unused imports warnings

## 0.2.13

- Project rewrite
- Internal cleanup

## 0.2.12

- Refactor command implementations to reduce boilerplate
- Add new CLI flags support
- Fix changelog formatting

## 0.2.10

- Clean up version show JSON output

## 0.2.9

- Fix clippy warnings (argument bundling, test module ordering)

## 0.2.8

- docs: homeboy docs outputs raw markdown by default
- changelog: homeboy changelog outputs raw markdown (removed show subcommand)

## 0.2.7

- Default JSON output envelope; allow interactive passthrough
- Require stdin+stdout TTY for interactive passthrough commands
- Standardize `--json` input spec handling for subcommands that support it (`project create --json`, `changelog --json`)
- Fix changelog finalization formatting

## 0.2.5

- added overlooked config command back in
- docs updated
- module standardized data contract

## 0.2.4

- Restore 'homeboy config' command wiring
- Update command docs to include config

## 0.2.3

- Fix changelog finalize placing ## Unreleased at top instead of between versions
- Fix changelog item insertion removing extra blank lines between items

## 0.2.2

- Add scan_json_dir<T>() helper to json module for directory scanning
- Refactor config list functions to use centralized json helpers
- Refactor module loading to use read_json_file_typed()
- Internal refactor

## 0.2.1

- Default app config values are serialized (no more Option-based defaults for DB settings)
- DB commands now read default CLI path/host/port from AppConfig instead of resolve helpers

## 0.2.0

### Improvements
- **Config schema**: Introduce `homeboy config` command group + `ConfigKeys` schema listing to standardize how config keys are described/exposed.
- **Config records**: Standardize config identity via `slugify_id()` + `SlugIdentifiable::slug_id()` and enforce id/name consistency in `ConfigManager::save_server()` and `ConfigManager::save_component()`.
- **App config**: Extend `AppConfig` with `installedModules: HashMap<String, InstalledModuleConfig>`; each module stores `settings: HashMap<String, Value>` and optional `sourceUrl` (stored in the module manifest).
- **Module scoping**: Add `ModuleScope::{effective_settings, validate_project_compatibility, resolve_component_scope}` to merge settings across app/project/component and validate `ModuleManifest.requires` (for example: `components`).
- **Module execution**: Tighten `homeboy module run` to require an installed/configured entry and resolve project/component context when CLI templates reference project variables.
- **Command context**: Refactor SSH/base-path resolution to shared context helpers (used by `db`/`deploy`) for more consistent configuration errors.
- **Docs**: Normalize docs placeholders (`<project_id>`, `<server_id>`, `<component_id>`) across embedded CLI documentation.

## 0.1.13

### Improvements
- **Changelog**: `homeboy changelog add` auto-detects changelog path when `changelogTargets` is not configured.
- **Changelog**: Default next section label is `Unreleased` (aliases include `[Unreleased]`).
- **Version**: `homeboy version bump` finalizes the "next" section into the new version section whenever `--changelog-add` is used.

## 0.1.12

### Improvements
- **Changelog**: Promote `homeboy changelog` from a shortcut to a subcommand group with `show` and `add`.
- **Changelog**: Add `homeboy changelog add <component_id> <message>` to append items to the “next” section (defaults to `Unreleased`).
- **Changelog**: Auto-detect changelog path (`CHANGELOG.md` or `docs/changelog.md`) when `changelogTargets` is not configured.
- **Config**: Support `changelogTargets` + `changelogNextSectionLabel`/`changelogNextSectionAliases` at component/project/app levels.
- **Version**: Write JSON version bumps via the `version` key (pretty-printed) when using the default JSON version pattern.
- **Deploy**: Load components via `ConfigManager` instead of ad-hoc JSON parsing.

## 0.1.11

### Improvements
- **Docs**: Expanded `docs/index.md` to include configuration/state directory layout and a clearer documentation index.
- **Docs/Positioning**: Refined README messaging to emphasize Homeboy’s LLM-first focus.

## 0.1.10

### Improvements
- **Modules**: Added git-based module workflows: `homeboy module install`, `homeboy module update`, and `homeboy module uninstall`.
- **Modules**: Added `.install.json` metadata (stored inside each module directory) to enable reliable updates from the original source.
- **Docs/Positioning**: Updated README and docs index to reflect LLM-first focus and Homeboy data directory layout.

## 0.1.9

### Improvements
- **Project management**: Added `homeboy project list` and `homeboy project pin` subcommands to manage pinned files/logs per project.
- **Config correctness**: Project configs are a strict `ProjectRecord` (`id` derived via `slugify_id(name)`) with validation to prevent mismatched IDs and to clear `active_project_id` when a project is deleted.
- **Docs**: Updated embedded docs to reflect new/removed commands.

## 0.1.8

### Improvements
- **Versioning**: `versionTargets` are now first-class for component version management (supports multiple files and multiple matches per file, with strict validation).
- **Deploy**: Reads the component version from `versionTargets[0]` for local/remote comparisons.

## 0.1.7

### Improvements
- **Component configuration**: Support `versionTargets` (multiple version targets) and optional `buildCommand` in component config.
- **Version bumping**: `homeboy version bump` validates that all matches in each target are the same version before replacing.
- **Deploy JSON output**: Deploy results include `artifactPath`, `remotePath`, `buildCommand`, `buildExitCode`, and an upload exit code for clearer automation.
- **Docs refresh**: Updated command docs + JSON output contract; removed outdated command/contract doc.

## 0.1.6

### New Features
- **Embedded docs**: Embed `homeboy/docs/**/*.md` into the CLI binary at build time, so `homeboy docs` works in Homebrew/releases.
- **Docs source of truth**: Keep CLI documentation under `homeboy/docs/` and embed it into the CLI binary.

- **Docs topic listing**: `available_topics` is now generated dynamically from embedded keys (newline-separated).

## 0.1.5

### Breaking Changes
- **Docs Command Output**: `homeboy docs` now prints embedded markdown to stdout by default (instead of paging).

### New Features
- **Core Path Utilities**: Added `homeboy_core::base_path` helpers for base path validation and remote path joining (`join_remote_path`, `join_remote_child`, `remote_dirname`).
- **Core Shell Utilities**: Added `homeboy_core::shell::cd_and()` to build safe "cd && <cmd>" strings.
- **Core Token Utilities**: Added `homeboy_core::token` helpers for case-insensitive identifiers and doc topic normalization.

### Improvements
- **Unified JSON Output**: CLI commands now return typed structs and are serialized in `crates/homeboy/src/main.rs`, standardizing success/error output and exit codes.
- **Docs & Skill Updates**: Updated documentation and the Homeboy skill.

## 0.1.4

### New Features
- **Build Command**: New `homeboy build <component>` for component-scoped builds
  - Runs a component build in its `local_path`

### Improvements
- **Version Utilities**: Refactored version parsing to shared `homeboy` core library
  - `parse_version`, `default_pattern_for_file`, `increment_version` now in core
  - Enables future reuse across CLI components

## 0.1.3

### New Features
- **Version Command**: New `homeboy version` command for component-scoped version management
  - `show` - Display current version from component's version_file
  - `bump` - Increment version (patch/minor/major) and write back to file
  - Auto-detects patterns for .toml, .json, .php files

## 0.1.2

### New Features
- **Git Command**: New `homeboy git` command for component-scoped git operations
  - `status` - Show git status for a component
  - `commit` - Stage all changes and commit with message
  - `push` - Push local commits to remote (with `--tags` flag support)
  - `pull` - Pull remote changes
  - `tag` - Create git tags (lightweight or annotated with `-m`)

### Improvements
- **Dogfooding Support**: Homeboy can now manage its own releases via git commands

## 0.1.1

### Breaking Changes
- **Config Rename**: `local_cli` renamed to `local_environment` in project configuration JSON files.

### Improvements
- **Deploy Command**: Improved deployment workflow.
- **Module Command**: Enhanced CLI module execution with better variable substitution.
- **PM2 Command**: Improved PM2 command handling for Node.js projects.
- **WP Command**: Improved WP-CLI command handling for WordPress projects.

## 0.1.0

Initial release.
- Project, server, and component management
- Remote SSH operations (wp, pm2, ssh, db, file, logs)
- Deploy and pin commands
- CLI module execution
- Shared configuration across clients
