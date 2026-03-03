# Homeboy

Config-driven development and deployment CLI with structured JSON output, embedded docs, and predictable contracts — built for AI agents and automation.

## What It Does

Homeboy manages the relationship between **components** (plugins, themes, CLIs, packages), **projects** (sites, applications), and **servers** (machines). You define these as JSON config. Then you deploy, version, release, audit, refactor, and operate across all of them from one tool.

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  COMPONENT  │────▶│   PROJECT   │────▶│   SERVER    │
│  Plugin,    │     │  Site or    │     │  VPS, host, │
│  theme, CLI │     │  application│     │  cloud...   │
└─────────────┘     └─────────────┘     └─────────────┘
                          │
                    ┌─────┴─────┐
                    │   FLEET   │
                    │  Group of │
                    │  projects │
                    └───────────┘
```

- **Components** are buildable/deployable units with version targets, build commands, and remote paths.
- **Projects** are deployment targets — a site on a server with a set of linked components.
- **Servers** are machines with SSH connection details.
- **Fleets** group projects for batch operations.
- **Extensions** add platform-specific behavior (WordPress, Node.js, Rust, etc.).

## Output Contract

Every command returns a stable JSON envelope:

```json
{
  "success": true,
  "data": { ... }
}
```

On failure:

```json
{
  "success": false,
  "error": {
    "code": "config.not_found",
    "message": "Component 'my-plugin' not found",
    "details": {},
    "hints": ["Run 'homeboy component list' to see available components"],
    "retryable": false
  }
}
```

Error codes are stable and namespaced (`config.*`, `ssh.*`, `deploy.*`, `git.*`, `internal.*`). Exit codes map to error categories. `data` is always the command-specific payload — each command documents its own shape.

Exceptions: `homeboy docs` outputs raw markdown. `homeboy ssh` and `homeboy logs --follow` use interactive passthrough.

## Discoverability

Everything is discoverable at runtime. No need to read source code or external documentation.

```bash
homeboy docs list                        # All embedded doc topics
homeboy docs commands/deploy             # Full deploy command reference
homeboy docs schemas/component-schema    # Component JSON schema
homeboy docs architecture/release-pipeline  # How release pipelines work
homeboy docs developer-guide/architecture-overview  # System internals
```

Topics cover commands, schemas, architecture, and developer guides — all embedded in the binary at compile time.

## Commands

| Command | What it does |
|---------|-------------|
| `deploy` | Push components to projects. Supports single project, multi-project (`--projects`), fleet (`--fleet`), or shared component (`--shared`) deployment. Builds once, deploys to many. `--check` and `--dry-run` for planning. |
| `release` | Executes a release pipeline: version bump → changelog finalize → build → git commit → git tag → push. Configurable steps with dependency resolution. Extension-backed steps supported. `--dry-run` to preview the plan. |
| `version` | Semantic version management. Detects versions from configured file targets using regex patterns. `bump` for patch/minor/major. |
| `changelog` | Add categorized entries (Feature, Fix, Breaking, Docs, Chore). Finalize for release. Entries stored in `docs/CHANGELOG.md`. |
| `changes` | Show commits and diffs since last version tag. Works per-component or per-project. `--git-diffs` for full diff output. |
| `status` | Actionable overview: which components have uncommitted changes, need a version bump, or are ready to deploy. Filters: `--uncommitted`, `--needs-bump`, `--ready`. |
| `refactor` | Structural renaming across a codebase. Standard mode generates case variants (lowercase, PascalCase, UPPER_CASE, plural, snake_case compounds). Literal mode for exact string matching. Collision detection. Dry-run by default. |
| `audit` | Discover code conventions from a codebase and flag drift. `--conventions` to see discovered patterns. `--fix --write` to apply corrections. `--baseline` to save state for future comparisons. |
| `cleanup` | Detect config drift, stale state, and hygiene issues. Filter by `--severity` (error/warning/info) or `--category` (local_path/remote_path/version_targets/extensions). |
| `build` | Build a component using its configured build command. |
| `test` | Run tests for a component. |
| `lint` | Lint a component. |
| `git` | Git operations with component awareness — status, commit, push, pull scoped to components. |
| `ssh` | Managed SSH connections to configured servers. |
| `file` | Remote file operations: list, read, write, find, grep. |
| `db` | Remote database queries, search, and tunneling. Passwords stored in OS keychain. |
| `logs` | Remote log viewing and searching. `--follow` for live tailing. |
| `transfer` | Transfer files between servers or local ↔ server. Supports recursive, compression, exclude patterns, dry-run. |
| `fleet` | Create and manage named groups of projects for coordinated operations. |
| `auth` | Authenticate with project APIs. Tokens stored in OS keychain. `login`, `logout`, `status`. |
| `api` | HTTP requests to project APIs. GET, POST, PUT, PATCH, DELETE. Uses stored auth automatically. |
| `component` | CRUD for component configs. `show`, `list`, `create`, `set`, `shared` (list projects using a component). |
| `project` | CRUD for project configs. `show`, `list`, `create`, `set`, `components add/remove`. |
| `server` | CRUD for server configs. SSH key generation and management. |
| `extension` | Install, list, update, and manage extensions. |
| `config` | View and modify global Homeboy settings. |
| `docs` | Read embedded documentation. `audit` to verify docs match code. `map` for machine-optimized codebase maps. `generate` for bulk doc creation from JSON spec. |
| `init` | Read-only environment discovery. Returns actionable status: what's ready to deploy, what needs a version bump, config gaps. |
| `upgrade` | Upgrade Homeboy to the latest version. |

Extensions add top-level commands at runtime. With the WordPress extension installed, `homeboy wp my-site plugin list` runs WP-CLI on the remote server. Rust extension adds `homeboy cargo`. Node.js adds `homeboy pm2`.

## Hooks

Components and extensions can declare lifecycle hooks:

| Event | When | Failure mode |
|-------|------|-------------|
| `pre:version:bump` | After version files updated, before git commit | Fatal |
| `post:version:bump` | After pre-bump hooks, before git commit | Fatal |
| `post:release` | After release pipeline completes | Non-fatal |
| `post:deploy` | After deploy completes on remote | Non-fatal |

Hooks are shell commands executed in the component's local path. Extension hooks run first, then component hooks.

## Secrets

Homeboy never stores secrets in config files. All credentials use OS-native keychain:

- **macOS**: Keychain Access
- **Linux**: libsecret / gnome-keyring
- **Windows**: Credential Manager

Stored secrets: API tokens (`homeboy auth`), database passwords, SSH key passphrases. Retrieved automatically when needed.

## Extensions

Extensions add platform-specific behavior. Installed from git repos, stored in `~/.config/homeboy/extensions/`.

| Extension | What it provides |
|-----------|-----------------|
| `wordpress` | WP-CLI integration, WordPress-aware build/test/lint, post-deploy hooks (activate plugin, flush cache) |
| `nodejs` | PM2 process management |
| `rust` | Cargo integration, crates.io publishing, release artifact packaging |
| `github` | GitHub release publishing |
| `homebrew` | Homebrew tap publishing |
| `swift` | Swift testing infrastructure for macOS, iOS, and Swift CLI projects |
| `openclaw` | AI agent platform management |
| `sweatpants` | Bridge to Sweatpants automation engine |
| `plasma-shield` | Network security control for AI agent fleets |

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id wordpress
homeboy extension list
```

Extensions can provide: CLI commands, release pipeline steps, platform behaviors (version detection, database, deployment), hooks, actions, and documentation topics.

Extension versioning supports constraint matching (`^1.0`, `>=2.0`, `~1.2`).

Browse available extensions: [homeboy-extensions](https://github.com/Extra-Chill/homeboy-extensions)

## GitHub Action

[homeboy-action](https://github.com/Extra-Chill/homeboy-action) is a composite GitHub Action that runs Homeboy in CI. It installs the binary from GitHub Releases, sets up extensions, registers your component, and runs any Homeboy commands — lint, test, audit, build.

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    component: my-plugin
    extensions: wordpress
    commands: |
      lint
      test
      audit
```

Results are posted as a PR comment with per-command status, auto-updated on re-runs.

## Configuration

All config lives in `~/.config/homeboy/`. No repo-local config files.

```
~/.config/homeboy/
├── homeboy.json       # Global defaults
├── components/        # Component definitions (JSON)
├── projects/          # Project definitions (JSON)
├── servers/           # Server connections (JSON)
├── fleets/            # Fleet definitions (JSON)
├── extensions/        # Installed extensions
├── keys/              # SSH keys
└── backups/           # Config backups
```

Full schemas: `homeboy docs schemas/component-schema`, `homeboy docs schemas/project-schema`, etc.

## Installation

```bash
# Homebrew (macOS/Linux)
brew tap Extra-Chill/homebrew-tap
brew install homeboy

# From source (requires Rust toolchain)
git clone https://github.com/Extra-Chill/homeboy.git
cd homeboy && cargo install --path .
```

## Documentation

All documentation is embedded in the binary and accessible via `homeboy docs`:

```bash
homeboy docs list                                    # Browse all topics
homeboy docs commands/commands-index                 # Full command reference
homeboy docs schemas/component-schema                # Config schemas
homeboy docs architecture/release-pipeline           # System internals
homeboy docs developer-guide/architecture-overview   # Architecture overview
```

## License

MIT License
Created by Chris Huber
https://chubes.net
