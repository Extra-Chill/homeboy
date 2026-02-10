# Homeboy CLI

Homeboy is a development and deployment automation CLI designed for agentic coding sessions and rapid iteration across multiple projects—whether local development or cloud network administration.

It standardizes configuration-driven workflows for projects, servers, components, and fleets while providing extensible automation through modules. Most commands return stable JSON output for machine parsing alongside human-readable output.

**Use cases:**
- **Local development**: Version management, builds, deployments for single projects
- **Multi-project workflows**: Shared components deployed across multiple sites
- **Network administration**: Fleet management, coordinated deployments, version sync across servers

**Note:** This is still early in development. Breaking changes may occur between releases.

## Capabilities

Homeboy provides these core capabilities:

- **Configuration & Context**: Manage projects, components, servers, fleets, and modules with `homeboy init` for environment discovery
- **Deployment**: Version-aware deployment with pre-flight checks, dry-run mode, and version comparison
- **Fleet Management**: Group projects into fleets, detect shared components, deploy updates across multiple sites
- **Release Pipelines**: Local orchestration replacing CI/CD with configurable steps and module-backed actions
- **SSH & Remote Operations**: Managed SSH connections, remote file operations, log management, and database access
- **Git Integration**: Status, commit (with granular file control), push/pull, and automatic tagging
- **Database Operations**: Table management, querying, search, and SSH tunneling
- **API Operations**: HTTP requests with keychain authentication per project
- **Version & Changelog**: Semantic versioning, changelog management, and changelog finalization
- **Documentation**: Embedded docs, codebase analysis, and bulk documentation generation
- **Module System**: Extensible runtime tools, platform behaviors, and custom release steps

## Agent usage (recommended)

Use the [Agent Hooks module](https://github.com/Extra-Chill/homeboy-modules/tree/main/agent-hooks) to guide your coding agent to use Homeboy effectively. Our module provides hooks for Claude Code and OpenCode. 

## `homeboy init`

`homeboy init` is a setup helper intended to reduce first-run friction. Depending on your environment it can guide you through (or scaffold) common configuration so you can start using commands like `project`, `server`, `ssh`, and modules sooner.

See: `homeboy docs commands/init`.

## Installation

See the monorepo-level documentation for installation options:

- [Root README](../README.md)
- [Homebrew tap](../homebrew-tap/README.md)

## Usage

See [CLI documentation](docs/index.md) for the authoritative command list, embedded docs topics (`homeboy docs ...`), and JSON contracts.

### Initial Setup Workflow

Start with `homeboy init` to understand your environment and get context. Based on the output:

1. **Create server**: `homeboy server create <id> --host <host> --user <user>`
2. **Create project**: `homeboy project create <id> <domain> --server <server-id>`
3. **Create component**: `homeboy component create <name> --local-path <path> --remote-path <path> --build-artifact <path>`
4. **Link components**: `homeboy project components add <project> <component>`
5. **Verify**: `homeboy init`

### Getting Started

```bash
# Initialize and explore
homeboy init                            # Get context and next steps
homeboy docs list                       # List all available topics
homeboy docs commands/commands-index     # Browse all commands

# Discovery commands
homeboy project list
homeboy component list
homeboy server list
homeboy module list
homeboy status                         # Show current working context (alias for init)
```

### Component and Project Setup Workflow

```bash
# Create a component (buildable/deployable unit)
homeboy component create <name> --local-path . --remote-path <path> --build-artifact <path>
homeboy component set <id> --build-command "./build.sh"
homeboy component set <id> --changelog-target "CHANGELOG.md"

# Create a project (deployable environment)
homeboy project create <name> <domain> --server <server_id>
homeboy project create <name> <domain> --base-path <local-path>  # Local project

# Link components to a project
homeboy project components add <project> <component>
homeboy project components set <project> <component-1> <component-2>
```

### Deployment Workflow

```bash
# Check component status without deploying
homeboy deploy <project> --check
homeboy deploy <project> --check --outdated

# Preview what would be deployed
homeboy deploy <project> --all --dry-run

# Deploy components
homeboy deploy <project> component-a component-b
homeboy deploy <project> --all
homeboy deploy <project> --outdated
```

### Fleet Management

Fleets are named groups of projects for coordinated operations. Use them to manage shared components across multiple sites.

```bash
# See which projects share components
homeboy component shared
# → my-plugin: [site-a, site-b, site-c]

# Create a fleet from projects
homeboy fleet create production --projects site-a,site-b,site-c
homeboy fleet add production --project site-d

# View fleet status
homeboy fleet list
homeboy fleet show production
homeboy fleet status production          # Component versions across fleet
homeboy fleet components production      # Component usage in fleet

# Deploy to a fleet
homeboy deploy my-plugin --fleet production

# Deploy to ALL projects using a component (auto-detected)
homeboy deploy my-plugin --shared
```

**Fleet use cases:**
- Keep plugins in sync across a network of WordPress sites
- Deploy theme updates to all sites using that theme
- Update CLI tools (like homeboy itself) across all servers
- Coordinate deployments between staging/production environments

### Release Workflow

```bash
# Review changes since last release
homeboy changes <component_id>

# Add changelog entries
homeboy changelog add <component_id> "Added: new feature"
homeboy changelog add <component_id> -m "Fixed: bug" -t fixed

# Plan and run release pipeline
homeboy release plan <component_id>
homeboy release run <component_id>
```

### Module Workflow

```bash
# List and install modules
homeboy module list
homeboy module install <git-url>
homeboy module update <module-id>
homeboy module uninstall <module-id>

# Run modules with context
homeboy module run <module-id> --project <project-id>
homeboy module run <module-id> --project <project-id> --component <component-id>
homeboy module run <module-id> --input key=value

# Execute module actions
homeboy module action <module-id> <action-id> --project <project-id>

# Configure module settings
homeboy project set <project-id> --json '{"modules": {"<module-id>": {"settings": {"key": "value"}}}}'
homeboy component set <id> --json '{"modules": {"<module-id>": {"settings": {"key": "value"}}}}'
```

### Git Workflow

```bash
# Git operations
homeboy git status <component_id>
homeboy git commit <component_id> -m "Update docs" --files README.md
homeboy git commit <component_id> -m "Release prep" --staged-only
homeboy git push <component_id> --tags
homeboy git pull <component_id>

# Bulk operations
homeboy git status --json '{"component_ids": ["id1", "id2"]}'
homeboy git commit --json '{"components": [{"id": "id1", "message": "msg"}]}'

# Version management
homeboy version show <component_id>
homeboy version bump <component_id> patch
homeboy version set <component_id> 1.2.3
```

### Remote Operations Workflow

```bash
# SSH access
homeboy ssh <project_id>               # Interactive shell
homeboy ssh <project_id> "ls -la"      # Execute command
homeboy ssh list                        # List available servers

# File operations
homeboy file list <project_id> <path>
homeboy file read <project_id> <path>
homeboy file write <project_id> <path> < stdin
homeboy file find <project_id> <path> --name "*.php" --type f --max-depth 3
homeboy file grep <project_id> <path> "TODO" --name "*.php" -i

# Log operations
homeboy logs list <project_id>
homeboy logs show <project_id> <path> --lines 100
homeboy logs show <project_id> <path> --follow
homeboy logs search <project_id> <path> "error" -i -C 3

# Database operations
homeboy db tables <project_id>
homeboy db describe <project_id> <table>
homeboy db query <project_id> "SELECT * FROM wp_posts LIMIT 10"
homeboy db search <project_id> <table> --column user_email --pattern "gmail" --exact
homeboy db delete-row <project_id> <table> <row_id>
homeboy db drop-table <project_id> <table>
homeboy db tunnel <project_id>
```

### Platform-Specific Tools (via modules)

```bash
# WordPress (via wordpress module)
homeboy wp <project_id> plugin list
homeboy wp <project_id> db export

# Node.js (via nodejs module)
homeboy pm2 <project_id> status
homeboy pm2 <project_id> restart

# Cargo (via rust module)
homeboy cargo <component_id> test
```

## Docs

- [CLI docs index](docs/index.md) - Command reference, embedded docs topics, and JSON contracts
- [Commands index](docs/commands/commands-index.md) - Complete list of built-in commands
- [JSON output contract](docs/json-output/json-output-contract.md) - Stable JSON envelope format
- [homeboy-modules repository](../homeboy-modules/) - Public modules and module creation guide

## Configuration

Configuration and state live under universal directory `~/.config/homeboy/` (all platforms):

- **All platforms**: `~/.config/homeboy/` (Windows: `%APPDATA%\homeboy\`)
- `projects/<id>.json`, `servers/<id>.json`, `components/<id>.json` - Persistent configuration
- `modules/<module_id>/<module_id>.json` - Module manifests
- `keys/` - SSH keys
- `homeboy/homeboy.json` - Global defaults

Homeboy does not use repo-local config files. All configuration is managed in the OS config directory.

## Module System

Homeboy modules extend functionality with platform-specific tools and behaviors. Modules can provide CLI tools, define platform behaviors, implement release steps, and include their own documentation.

### Extensibility Model

The module system enables deep extensibility without modifying the core CLI:

- **Runtime Tools**: Integrate any CLI tool (WP-CLI, PM2, Cargo, GitHub CLI, etc.) as a first-class command
- **Platform Behaviors**: Define database schemas, deployment patterns, and version detection rules
- **Release Steps**: Implement custom pipeline steps that integrate with the core release workflow
- **Documentation**: Modules provide their own embedded documentation that integrates with `homeboy docs`
- **Commands**: Modules can register top-level CLI commands alongside built-in commands

Modules are JSON manifests that define configuration and runtime behavior. See the [homeboy-modules repository](../homeboy-modules/) for available modules, examples, and implementation details.

### Public Module Repository

The official [homeboy-modules](../homeboy-modules/) repository contains publicly available modules:

- **wordpress**: WP-CLI integration with database discovery
- **nodejs**: PM2 process management
- **rust**: Cargo CLI integration
- **github**: GitHub CLI for issues, PRs, and repos
- **homebrew**: Homebrew tap publishing

Install modules from this repository or create your own to extend Homeboy for your specific workflows.

### Creating Modules

To create a custom module:

1. Create a directory structure with a JSON manifest
2. Define runtime behavior, platform behaviors, and/or release actions
3. Install locally via symlink for development: `homeboy module install <local-path>`
4. Publish to a git repository for sharing

See the [homeboy-modules README](../homeboy-modules/README.md) for detailed module creation guidelines.

### Installing Modules

```bash
# From git repository
homeboy module install <git-url>

# From local path (creates symlink for development)
homeboy module install <local-path>

# Update a git-cloned module
homeboy module update <module-id>

# Uninstall a module
homeboy module uninstall <module-id>
```

### Module Manifest

Each module includes a JSON manifest (`<module_id>/<module_id>.json`) defining:

- **Runtime**: `run_command`, `setup_command`, `ready_check`, `entrypoint`, `env`
- **Platform**: Database CLI templates, version patterns, deployment behaviors
- **Release Actions**: Custom pipeline steps (e.g., `release.publish`)
- **Commands**: Additional top-level CLI commands provided by the module
- **Docs**: Embedded documentation topics

Example runtime configuration:

```json
{
  "runtime": {
    "run_command": "./venv/bin/python3 {{entrypoint}} {{args}}",
    "setup_command": "python3 -m venv venv && ./venv/bin/pip install -r requirements.txt",
    "ready_check": "test -f ./venv/bin/python3",
    "entrypoint": "main.py",
    "env": {
      "MY_VAR": "{{modulePath}}/data"
    }
  }
}
```

### Module Settings

Module settings are merged across scopes (later scopes override earlier ones):

1. Project (`projects/<project_id>.json`): `modules.<module_id>.settings`
2. Component (`components/<component_id>.json`): `modules.<module_id>.settings`

Effective settings are passed to modules via environment variables:

- `HOMEBOY_SETTINGS_JSON`: Merged effective settings (JSON)
- `HOMEBOY_PROJECT_ID`: Project ID (when project context is used)
- `HOMEBOY_COMPONENT_ID`: Component ID (when component context is resolved)
- `HOMEBOY_COMPONENT_PATH`: Absolute path to component directory

Configure module settings:

```bash
# At project level
homeboy project set <project-id> --json '{"modules": {"<module-id>": {"settings": {"key": "value"}}}}'

# At component level
homeboy component set <component-id> --json '{"modules": {"<module-id>": {"settings": {"key": "value"}}}}'
```

### Module Runtime

Modules can define their own top-level CLI commands and documentation topics. Discover what's available:

```bash
homeboy docs list                    # List all topics (core + modules)
homeboy docs <module-topic>          # Render module-provided docs
```

Executable modules define runtime behavior:

- **run_command**: Shell command to execute the module (supports template variables)
- **setup_command**: Optional command run during install/update
- **ready_check**: Optional command to verify module readiness
- **env**: Optional environment variables to set

Template variables available in run_command:
- `{{modulePath}}`: Module directory path
- `{{entrypoint}}`: Module entrypoint file
- `{{args}}`: Command-line arguments passed to module
- Project context variables (when available)

### Module Actions

Modules can define actions for execution via `homeboy module action`:

```bash
homeboy module action <module-id> <action-id> --project <project-id> --data '<json>'
```

Actions can be:
- **CLI type**: Execute module runtime commands
- **API type**: Make HTTP requests to project APIs

Actions are also used in release pipelines as custom steps:

```json
{
  "id": "publish",
  "type": "module.run",
  "config": {
    "module": "github",
    "inputs": [
      {"id": "create_release", "value": "true"}
    ]
  }
}
```

## License

MIT License
Created by Chris Huber
https://chubes.net
