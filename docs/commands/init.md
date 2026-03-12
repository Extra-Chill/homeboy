# `homeboy init`

Deprecated compatibility alias for `homeboy status --full`. **Read-only** - creates no files or state.

**Prefer:** `homeboy status --full`

## Usage

```bash
homeboy init
homeboy status --full
```

## When You Need It

- **First time with a repository** - Discover what components are configured
- **Before release operations** - Check component status, unreleased commits, version alignment
- **AI agent workflows** - Structured JSON context for automation
- **Debugging** - See baseline warnings, unconfigured version patterns, config gaps

## When You Don't Need It

If you already know your component IDs, run commands directly:

```bash
homeboy changes my-plugin      # See unreleased changes
homeboy version bump my-plugin # Bump version
homeboy deploy my-plugin       # Deploy
```

Most commands work without running `init` first. The `init` command is purely informational - it reads existing configuration but never modifies anything.

## Getting Started

Run `homeboy status --full` to gather all context in one call:
- Current directory state (managed, components, gaps)
- Available servers, projects, components, extensions
- Version alignment warnings and unconfigured patterns

Then read workspace docs (CLAUDE.md, README.md) for project context.

## Output Structure

```json
{
  "success": true,
  "data": {
    "command": "status",
    "context": {
      "cwd": "/path/to/repo",
      "git_root": "/path/to/repo",
      "managed": true,
      "matched_components": ["component-id"],
      "contained_components": [],
      "project": { "id": "project-id", "domain": "example.com" },
      "components": [{ "id": "...", "build_artifact": "...", "gaps": [...] }],
      "suggestion": "Run homeboy deploy..."
    },
    "next_steps": [
      "Read CLAUDE.md and README.md for repo-specific guidance.",
      "Run `homeboy docs documentation/index` for Homeboy documentation.",
      "Run `homeboy docs commands/commands-index` to browse available commands."
    ],
    "servers": [
      { "id": "server-id", "host": "...", "user": "...", "port": 22 }
    ],
    "projects": [
      { "id": "project-id", "domain": "example.com" }
    ],
    "components": [
      {
        "id": "component-id",
        "local_path": "...",
        "remote_path": "...",
        "build_artifact": "...",
        "build_command": "./build.sh",
        "version_targets": [{ "file": "plugin.php", "pattern": "..." }]
      }
    ],
    "extensions": [
      {
        "id": "extension-id",
        "name": "...",
        "version": "...",
        "ready": true,
        "ready_reason": null,
        "ready_detail": null,
        "compatible": true
      }
    ],
    "version": {
      "component_id": "component-id",
      "version": "0.20.7",
      "targets": [{ "file": "Cargo.toml", "pattern": "...", "full_path": "...", "match_count": 1 }]
    },
    "git": {
      "branch": "main",
      "clean": true,
      "ahead": 0,
      "behind": 0,
      "commits_since_version": 5,
      "version_baseline": "v0.20.7"
    },
    "last_release": {
      "tag": "v0.20.7",
      "date": "2026-01-15",
      "summary": "Add -m flag for changelog add command"
    },
    "changelog": {
      "path": "docs/changelog.md",
      "label": "Unreleased",
      "items": ["Queued change"]
    },
    "agent_context_files": [
      "CLAUDE.md",
      "README.md",
      "docs/index.md"
    ]
  }
}
```

## Output Interpretation

| Field | Meaning |
|-------|---------|
| `context.managed` | true = repo has registered component(s) |
| `context.matched_components` | Components matching current path |
| `context.contained_components` | Components in subdirectories (monorepo) |
| `context.components[].gaps` | Missing config with remediation commands |
| `next_steps` | Actionable guidance for agents and onboarding |
| `servers`, `projects`, `components` | Available resources for reference |
| `extensions` | Available Homeboy extensions |
| `version` | Current component version snapshot (first matched component) |
| `git` | Branch + clean state + ahead/behind snapshot + release state |
| `git.commits_since_version` | Number of commits since last version tag (indicates unreleased work) |
| `git.version_baseline` | Tag or commit hash used as baseline for commit count |
| `last_release` | Latest changelog release summary |
| `changelog` | Unreleased preview from component changelog |
| `agent_context_files` | Git-tracked markdown files for AI agent context (CLAUDE.md, README.md, etc.) |

## Component Status

Each component has a `status` field indicating its release state:

| Status | Meaning |
|--------|---------|
| `clean` | No commits since last version - ready to deploy |
| `needs_bump` | Has code commits that require a version bump before release |
| `docs_only` | Only documentation changes since last version - no version bump needed |
| `uncommitted` | Has uncommitted changes in working directory |
| `unknown` | Could not determine release state |

### Commit Categorization

Components also include commit count breakdowns:

| Field | Meaning |
|-------|---------|
| `commits_since_version` | Total commits since last version tag |
| `code_commits` | Commits that touch code files (require version bump) |
| `docs_only_commits` | Commits that only touch documentation files (*.md, docs/*) |

Docs-only detection uses belt-and-suspenders approach:
1. **Fast path**: Commits with `docs:` prefix (conventional commit format)
2. **Fallback**: Commits where all changed files match docs patterns (*.md or docs/*)

## Decision Tree

### If `managed: true`
Repo is configured. Check for gaps and complete setup.

```bash
# Gaps include remediation commands - run them
homeboy component set <id> --build-command "./build.sh"
homeboy component set <id> --changelog-targets '["CHANGELOG.md"]'
```

### If `managed: false` with `containedComponents`
Monorepo root - components exist in subdirectories. Check gaps, skip creation.

### If `managed: false` (empty)
Create based on workspace docs:

**Project** (deployable environment with domain):
```bash
homeboy project create "<name>" <domain> --server <server_id> --extension <extension_id>
```

**Component** (buildable/deployable unit):
```bash
homeboy component create "<name>" --local-path "." --remote-path "<path>" --project <project_id>
homeboy component set <id> --build-command "./build.sh" --build-artifact "build/<name>.zip"
```

## Derivation Rules

1. **name**: Directory name or from workspace docs
2. **remotePath**: Match existing component patterns in target project
3. **buildArtifact/buildCommand**: From build.sh, Makefile, or workspace docs
4. **domain**: ASK (cannot derive locally)
5. **server_id**: Auto-select if only one exists

## Verification

```bash
homeboy status --full  # Confirm managed: true
```
