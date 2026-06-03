# Architecture Overview

Homeboy is headless automation for agentic software engineering workflows. It is
built in Rust with a config-driven, extension-oriented architecture that keeps
core generic while letting extensions provide platform-specific behavior.

## Design Principles

### Core Agnostic, Extensions Specific

Homeboy core owns the reusable automation substrate: command routing,
configuration, scope resolution, structured output, persisted runs, baselines,
release/evidence workflows, runner offload, daemon/API surfaces, and generic
audit primitives. It should not encode ecosystem-specific assumptions such as
Cargo behavior, WP-CLI flows, package-manager semantics, or framework-specific
audit rules.

Those semantics belong in extensions. The shared implementation lives in
[Extra-Chill/homeboy-extensions](https://github.com/Extra-Chill/homeboy-extensions),
and private teams can provide the same contracts through custom extension
manifests, scripts, hooks, docs, and CLI verbs.

### Single Source of Truth

Configuration is the authoritative source for system behavior. Project- and
platform-specific logic should be represented as component config, project
config, or extension contracts rather than hard-coded into core.

### Local-First

Homeboy is local-first: the normal developer loop runs from a checkout and
stores configuration under the Homeboy config directory. Remote operations,
runner offload, and the local daemon/API are optional headless surfaces for CI,
scheduled automation, and agent workflows; they do not replace the portable
repo-level `homeboy.json` loop.

### Extension System

Extensibility through an extension system that allows:
- Platform-specific behaviors such as Rust, WordPress, Node.js, GitHub,
  Homebrew, and Swift
- Custom CLI commands exposed as top-level verbs such as `homeboy wp` and `homeboy cargo`
- Release pipeline actions, deploy hooks, audit rules, lint/test/build runners,
  benchmark runners, and trace support
- Documentation topics embedded alongside extension behavior

### Configuration-Driven

All behavior is configurable via JSON:
- Projects, servers, components
- Extension manifests
- Release pipelines
- Extension settings per project/component

## Core Systems

### Configuration Management

**Location:** `src/core/config.rs`

Centralized configuration system that:
- Loads JSON configs from config directory
- Validates schemas
- Merges settings across scopes (project + component)
- Provides helpers for set/merge/remove operations

Config entities:
- **Components**: Buildable, testable, reviewable units; often declared portably
  in repo-level `homeboy.json`
- **Projects**: Deployable environments and server bindings
- **Servers**: SSH connection settings
- **Fleets**: Named groups of projects for batch inspection and operations
- **Rigs**: Reproducible local multi-component environments
- **Stacks**: Combined-fixes branch specifications
- **Extensions**: Domain-specific behaviors and tools

### Storage Layer

**Location:** `src/core/engine/local_files.rs`

File-based storage for configurations:
- Reads/writes JSON files in config directory
- Handles atomic operations for safety
- Cross-platform paths (macOS, Linux, Windows)

### Template System

**Location:** `src/core/engine/template.rs`

Variable substitution in templates:
- Both `{var}` and `{{var}}` syntax supported
- Context-aware resolution (project, component, extension variables)
- Used in: deploy commands, extension runtime, platform behaviors

### Execution System

**Location:** `src/core/engine/executor.rs` and `src/core/engine/`

Executes local, remote, and extension commands with:
- Environment variable injection
- Working directory management
- Output capture
- Exit code handling

Supports:
- Local commands such as builds, tests, benchmarks, and review gates
- Remote commands via SSH for configured projects and fleets
- Extension runtime execution
- Extension actions, release hooks, and CLI routing
- Lab offload for commands with portable runner contracts

### SSH Operations


SSH client wrapper that:
- Manages SSH connections
- Handles keychain-stored passphrases
- Supports SSH agent forwarding
- Executes remote commands and file operations

### HTTP Client

**Location:** `src/core/server/http.rs`

HTTP client for API operations:
- Template-based URL construction
- Keychain-stored authentication
- JSON request/response handling
- Extension action API integration

### Keychain Integration


Secure credential storage:
- macOS: Keychain Access
- Linux: libsecret/gnome-keyring
- Windows: Credential Manager

Secrets stored:
- API tokens (per project)
- Database passwords (per project)
- SSH key passphrases

### Git Operations

**Location:** `src/core/git/`

Git wrapper for:
- Status checking
- Committing changes
- Tagging releases
- Push/pull operations

### Version Management

**Location:** `src/core/extension/version.rs`

Semantic versioning:
- Pattern-based version detection in files
- Version bump operations (patch, minor, major)
- Multi-target version detection

### Changelog Management

**Location:** `src/core/release/changelog/`

Changelog operations:
- Add entries
- Categorize changes (Feature, Fix, Breaking, Docs, Chore, Other)
- Finalize for release
- Extract for git commits

### Extension System

**Location:** `src/core/extension/mod.rs`

Extension management:
- Install from git or local path
- Load manifests and declared capabilities
- Resolve settings across component/project scopes and CLI overrides
- Execute runtime scripts, actions, hooks, and extension-backed pipeline steps
- Provide CLI commands and docs without changing Homeboy core
- Declare structured sidecars that downstream agents can inspect as stable contracts

### Release Pipeline

**Location:** `src/core/release/` (executor.rs, pipeline.rs, types.rs)

Local orchestration system:
- Define steps as configuration
- Dependency graph resolution
- Plan without execution
- Execute with error handling

Step types:
- Core: preflight, changelog, version, git, deploy, cleanup, and artifact steps
- Extension-backed: prepare, package, and publish actions declared by extensions

### Code Audit

**Location:** `src/core/code_audit/`

Convention detection and drift analysis:
- Fingerprints source files (methods, registrations, types) via extensions
- Groups files by directory and language
- Discovers conventions (patterns most files follow)
- Detects outliers, structural complexity, duplication, dead code, test coverage gaps
- Baseline comparison for drift tracking
- Fix stub generation for outlier files

Audit pipeline phases:
1. Discovery (auto-discover file groups)
2. Convention detection
3. Convention checking
4. Findings (outliers, structural, duplication, dead code, test coverage)
5. Report (alignment score)
6. Cross-directory convention discovery

### Docs Audit

**Location:** `src/core/code_audit/docs_audit/`

Documentation verification:
- Extracts claims (file paths, directory paths) from markdown docs
- Verifies claims against the filesystem
- Detects features in source code and checks documentation coverage
- Identifies priority docs (source files changed since baseline tag)
- Supports `--features` for machine-readable feature inventory

### Fleet Management

**Location:** `src/core/fleet/`

Fleet management for grouped environments:
- Named groups of projects
- Shared component detection
- Fleet-wide status, triage, and deploy operations
- Coordinated operations across multiple servers or local environments

Config entities:
- **Fleets**: Named groups of projects for batch operations

### CLI Layer

**Location:** `src/commands/` and `src/main.rs`

Command-line interface built with `clap`:
- Command definitions and parsing
- Output mode selection (JSON, markdown, interactive)
- JSON envelope wrapping
- Response mapping

### Documentation System


Embedded documentation:
- Markdown files embedded at compile time (`build.rs`)
- Runtime topic resolution
- Extension-provided docs support

## Data Flow

### Deploy Command Flow

1. CLI parses `homeboy deploy <project> [components]`
2. Loads project configuration
3. Loads linked server configuration
4. Loads component configurations
5. Resolves deployment targets
6. For each component:
   - Detect version from local files
   - Detect version from remote files
   - Compare versions
   - If outdated or explicitly selected:
     - Execute build command
     - Upload artifact via SSH
     - Execute extract command
7. Return results in JSON envelope

### Extension Execution Flow

1. CLI parses `homeboy extension run <extension> --project <project> --component <component>`
2. Load extension manifest
3. Resolve project configuration (if provided)
4. Resolve component configuration (if provided)
5. Merge settings from project and component scopes
6. Build execution context:
   - Extension metadata
   - Project context (domain, paths)
   - Component context (paths)
   - Merged settings
7. Set environment variables
8. Execute `runtime.run_command` with template resolution
9. Capture output and exit code
10. Return results in JSON envelope

### Release Pipeline Flow

1. CLI parses `homeboy release <component>`
2. Load component configuration
3. Parse release pipeline steps
4. Validate step dependencies
5. Execute steps in order:
   - Wait for dependencies to complete
   - Execute step (build, extension, git, etc.)
   - Stop on failure
6. Return results with status for each step

## Extension Integration

### Extension Manifest

Extension manifest defines:
- Runtime configuration
- Platform behaviors (database, deployment, version patterns)
- CLI commands
- Actions (CLI or API)
- Release actions
- Documentation topics

### Extension Loading

Extensions are loaded from:
- Git-cloned directories in `~/.config/homeboy/extensions/`
- Symlinked local directories
- Extension manifest: `<extension_id>/<extension_id>.json`

### Extension Execution

Direct extension execution uses `homeboy extension run <extension_id>`. Other
Homeboy command families, such as lint, test, build, bench, trace, release, and
deploy, can also invoke extension-owned scripts or actions through their own
typed contracts.

## Error Handling

**Location:** `src/core/error/mod.rs`

Centralized error system:
- Error categories (validation, io, extension, etc.)
- Error context (file path, component ID, etc.)
- Error messages for CLI output
- Error conversion for JSON envelope

## Output System


Output modes:
- **JSON**: Machine-readable, wrapped in stable envelope
- **Markdown**: Human-readable documentation
- **Interactive**: Passthrough for TTY commands (SSH, logs)

JSON envelope structure:
```json
{
  "success": true|false,
  "data": {},
  "error": {}
}
```

## Cross-Platform Considerations

### Paths

Homeboy handles path differences:
- **macOS/Linux**: Unix-style paths (`/home/user/`)
- **Windows**: Windows-style paths (`C:\Users\user\`)
- Path expansion: `~` is expanded to home directory

### Config Directory

Universal config directory:
- **macOS**: `~/.config/homeboy/`
- **Linux**: `~/.config/homeboy/`
- **Windows**: `%APPDATA%\homeboy\`

### Keychain

OS-specific credential storage:
- **macOS**: Keychain Access framework
- **Linux**: libsecret or gnome-keyring
- **Windows**: Windows Credential Manager API

## Related

- [API client system](../architecture/api-client.md) - HTTP client details
- [Keychain/secrets management](../architecture/keychain-secrets.md) - Credential storage
- [SSH key management](../architecture/ssh-key-management.md) - SSH operations
- [Release pipeline system](../architecture/release-pipeline.md) - Release orchestration
- [Execution context](../architecture/execution-context.md) - Runtime context building
- [Config directory structure](./config-directory.md) - File organization
