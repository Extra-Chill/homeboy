---
name: homeboy
description: "Use Homeboy CLI for version management, deployment, fleet operations, documentation tooling, database ops, module management, and remote file/log access. Invoke for component versioning (bump, changelog, release), deploying to remote servers via SSH, managing fleets of projects, docs audit/scaffold/generate, running modules, and any task involving homeboy commands."
compatibility: "Cross-platform Rust CLI. Works with any language/framework. Requires SSH for remote operations."
---

# Homeboy CLI

Development and deployment automation. Generic — works with any language/framework.

## Entity Hierarchy

```
Component  →  versioned, deployable unit (plugin, CLI, module)
    ↓
Project    →  deployment target (site on a server, links to components)
    ↓
Server     →  SSH connection config

Fleet      →  named group of projects (for batch operations)
Module     →  installable extension (CLI scripts, actions, docs)
```

**Storage:** `~/.config/homeboy/{components,projects,servers,fleets}/<id>.json`
**Modules:** `~/.config/homeboy/modules/<id>/`

## Commands Overview

| Command | Purpose |
|---------|---------|
| `component` | Create, show, list, delete, rename components |
| `project` | Create, show, list, delete, rename projects + manage components/pins |
| `server` | Register, list, delete servers + SSH key management |
| `fleet` | Create, list, delete fleets + status/check/drift detection |
| `deploy` | Deploy components to projects, fleets, or all shared projects |
| `ssh` | SSH into a project's server |
| `file` | Remote file ops (ls, read, write, delete, rename, find, search, download) |
| `logs` | Remote log viewing (list, show, clear, search) |
| `db` | Database ops via SSH tunnel (list, describe, select, search, delete, drop, tunnel) |
| `transfer` | Transfer files between servers |
| `module` | List, run, install, update, uninstall, setup modules |
| `docs` | Topic display, scaffold, audit, generate documentation |
| `changelog` | Show, add entries, init changelogs |
| `version` | Show or bump versions |
| `release` | Plan release workflows |
| `build` | Build a component |
| `test` | Run tests for a component |
| `lint` | Lint a component |
| `git` | Git operations (status, commit, push, pull, tag) |
| `changes` | Show changes since last version tag |
| `auth` | Authenticate with project APIs |
| `api` | Make API requests (GET, POST, PUT, PATCH, DELETE) |
| `config` | Show, set, unset, reset global config |
| `init` | Get repo context and status (read-only, creates no state) |
| `upgrade` | Self-update Homeboy |

## Version + Release

```bash
# Add changelog entries (repeatable -m for multiple)
homeboy changelog add <component> -t Added -m "Feature description"

# Bump version (updates files, finalizes changelog, commits, tags, pushes)
homeboy version bump <component> minor   # 0.1.0 → 0.2.0
homeboy version bump <component> patch   # 0.1.0 → 0.1.1

# Plan a release (interactive workflow)
homeboy release <component>

# Show changes since last tag
homeboy changes <component>
```

Version targets are regex patterns — work on any file (Cargo.toml, package.json, PHP headers).

### Component Hooks

Components support lifecycle hooks for version/release operations:

- `pre_version_bump_commands` — run before version targets update (fatal on failure)
- `post_version_bump_commands` — run after version update, before git ops (fatal on failure)

```json
{
  "pre_version_bump_commands": ["cargo build --release"],
  "post_version_bump_commands": ["git add Cargo.lock"]
}
```

## Deploy

```bash
# Single project
homeboy deploy <project> <component>

# Check what would deploy (no build/upload)
homeboy deploy <project> <component> --check

# Deploy to fleet
homeboy deploy <component> --fleet <fleet-name>

# Deploy to ALL projects using component (auto-detect)
homeboy deploy <component> --shared
```

## Fleet Management

```bash
homeboy fleet create prod --projects site-a,site-b
homeboy fleet status prod           # component versions across fleet (local only)
homeboy fleet check prod            # drift detection (local vs remote)
homeboy fleet add prod site-c
homeboy fleet remove prod site-a
```

## Documentation Tooling

### Scaffold (analyze coverage)

```bash
homeboy docs scaffold <component> [--docs-dir <dir>]
```

Read-only analysis: reports source directories, existing docs, undocumented areas.

### Audit (validate docs)

```bash
homeboy docs audit <component>
```

Extracts claims from docs and verifies against codebase:
- **File/directory paths** — verified against filesystem
- **Code examples** — flagged for manual verification
- **Undocumented features** — scans changed files for registration patterns (configured via `audit_feature_patterns` in module manifests), cross-references against doc content

Claim statuses: `verified`, `broken`, `needs_verification`

**Agent workflow:**
1. Run `homeboy docs audit <component>`
2. For each task where `status != "verified"`: fix broken refs, verify flagged claims
3. Re-run audit to confirm resolution

### Generate (bulk create docs)

```bash
homeboy docs generate --json '<spec>'
homeboy docs generate @spec.json
homeboy docs generate -   # stdin
```

Spec format:
```json
{
  "output_dir": "docs",
  "files": [
    { "path": "engine.md", "content": "Full markdown..." },
    { "path": "handlers.md", "title": "Handler System" }
  ]
}
```

### Embedded Topics

```bash
homeboy docs list                        # show available topics
homeboy docs <topic>                     # render topic
homeboy docs documentation/generation    # doc writing guidelines
homeboy docs documentation/alignment     # keeping docs current
```

### Full Documentation Workflow

1. **Analyze**: `homeboy docs scaffold <component>`
2. **Learn**: `homeboy docs documentation/generation`
3. **Plan**: determine structure from analysis
4. **Generate**: `homeboy docs generate --json '<spec>'`
5. **Validate**: `homeboy docs audit <component>`
6. **Maintain**: `homeboy docs documentation/alignment`

## Module System

```bash
homeboy module list [-p <project>]              # list available modules
homeboy module show <module>                     # show module details
homeboy module run <module> [-p <project>] [-c <component>] [-i key=val]
homeboy module setup <module>                    # run module setup
homeboy module install <source> [--id <id>]      # install from git URL or local path
homeboy module update <module>                   # git pull for cloned modules
homeboy module uninstall <module>                # remove module
homeboy module action <module> <action>          # execute module action
homeboy module set --json '<json>'               # update module manifest
```

### Module Manifest Config

Module manifests (`module.yaml`) support:
- `audit_ignore_claim_patterns` — regex patterns for claims to ignore during docs audit
- `audit_feature_patterns` — regex patterns to detect feature registrations in source (e.g., `registerStepType\(\s*['"]\w+`, `register_ability\(\s*['"]\w+`). Audit flags matches with zero doc mentions as potentially undocumented.

## Remote Operations

### File Operations

```bash
homeboy file <project> ls [path]
homeboy file <project> read <path>
homeboy file <project> write <path>        # reads content from stdin
homeboy file <project> delete <path>
homeboy file <project> rename <from> <to>
homeboy file <project> find <pattern>
homeboy file <project> search <pattern>
homeboy file <project> download <path>
```

### Log Viewing

```bash
homeboy logs <project>                     # show all pinned logs
homeboy logs <project> <log-id>            # show specific log
homeboy logs <project> list                # list pinned log files
homeboy logs <project> search <pattern>
homeboy logs <project> clear <log-id>
```

### Database

```bash
homeboy db <project> tables                # list tables
homeboy db <project> describe <table>      # table structure
homeboy db <project> query <table>         # SELECT query
homeboy db <project> search <table> <col> <val>
homeboy db <project> delete-row <table> <id>
homeboy db <project> drop-table <table>
homeboy db <project> tunnel                # open SSH tunnel to DB
```

### SSH & Transfer

```bash
homeboy ssh <project>                      # interactive SSH session
homeboy ssh <project> -- <command>         # run remote command
homeboy transfer <source-project> <dest-project> <path>
```

## Git Operations

```bash
homeboy git status <component>
homeboy git commit <component> -m "message"
homeboy git push <component>
homeboy git pull <component>
homeboy git tag <component> <tag>
```

## API Client

```bash
homeboy auth login <project>               # authenticate
homeboy auth status <project>              # check auth
homeboy auth clear <project>               # clear credentials

homeboy api get <project> <endpoint>
homeboy api post <project> <endpoint> --json '<body>'
homeboy api put <project> <endpoint> --json '<body>'
homeboy api patch <project> <endpoint> --json '<body>'
homeboy api delete <project> <endpoint>
```

## Build & Test

```bash
homeboy build <component>
homeboy test <component>
homeboy lint <component>
```

## Configuration

```bash
homeboy config show                        # display merged config
homeboy config set <pointer> <value>       # set value at JSON pointer
homeboy config unset <pointer>             # remove value
homeboy config reset                       # reset to defaults
homeboy config path                        # show config file path
```

## Component Config Example

```json
{
  "local_path": "/path/to/repo",
  "remote_path": "/usr/local/bin",
  "build_command": "cargo build --release",
  "build_artifact": "target/release/myapp",
  "remote_owner": "www-data:www-data",
  "version_targets": [
    { "file": "Cargo.toml", "pattern": "^version = \"(\\d+\\.\\d+\\.\\d+)\"" }
  ],
  "changelog_target": "CHANGELOG.md",
  "pre_version_bump_commands": [],
  "post_version_bump_commands": []
}
```

## Common Patterns

**Self-deploying CLI:**
```bash
homeboy deploy homeboy --shared
```

**Multi-site plugin update:**
```bash
homeboy changelog add my-plugin -t Fixed -m "Bug fix"
homeboy version bump my-plugin patch
homeboy deploy my-plugin --fleet production
```

**Check drift before deploy:**
```bash
homeboy fleet check production
```

**Full docs audit cycle:**
```bash
homeboy docs scaffold my-component
homeboy docs audit my-component
# Fix broken refs from audit output
homeboy docs audit my-component  # re-verify
```

## JSON Output

All commands output structured JSON (except `docs` topic display and interactive modes). Standard envelope:

```json
{
  "success": true,
  "data": { ... },
  "hints": ["Human-readable notes"]
}
```

Errors:
```json
{
  "success": false,
  "error": "Error message",
  "hints": ["Suggestions"]
}
```
